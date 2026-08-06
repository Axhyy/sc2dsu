#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod config;
mod dsu;
mod gyro_calibration;
mod probe;
mod stats;
mod triton;
mod ui;

use hidapi::HidApi;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

const SAMPLE_QUEUE_LEN: usize = 64;

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Gui { start_minimized: bool },
    Headless,
    Probe,
}

fn parse_args() -> Mode {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(args: impl Iterator<Item = String>) -> Mode {
    for arg in args {
        match arg.as_str() {
            "--probe" | "-p" => return Mode::Probe,
            "--headless" | "-H" => return Mode::Headless,
            "--tray" | "--minimized" => {
                return Mode::Gui {
                    start_minimized: true,
                };
            }
            "--gui" => {
                return Mode::Gui {
                    start_minimized: false,
                };
            }
            _ => {}
        }
    }
    Mode::Gui {
        start_minimized: false,
    }
}

#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole};
    // SAFETY: AttachConsole/AllocConsole take no caller-supplied pointers; a failed
    // AttachConsole (no parent console, or already attached) is detected via its return
    // value, and AllocConsole's failure (e.g. console already present) is harmless here.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            AllocConsole();
        }
    }
}

#[cfg(not(windows))]
fn attach_console() {}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match parse_args() {
        Mode::Probe => {
            attach_console();
            probe::run()
        }
        Mode::Headless => {
            attach_console();
            run_server(None)
        }
        Mode::Gui { start_minimized } => run_server(Some(start_minimized)),
    }
}

fn run_server(gui_start_minimized: Option<bool>) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = config::load_or_create();
    let dsu_port = cfg.port;
    let dsu_expose = cfg.expose_to_network;
    config::install(cfg);

    let dsu_wants_device = Arc::new(AtomicBool::new(false));
    let ui_wants_device = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));
    let (tx, rx) = sync_channel::<triton::DeviceEvent>(SAMPLE_QUEUE_LEN);

    let device_handle = {
        let dsu_wants = dsu_wants_device.clone();
        let ui_wants = ui_wants_device.clone();
        let shutdown = shutdown.clone();
        thread::Builder::new()
            .name("controller-reader".into())
            .spawn(move || run_device_thread(dsu_wants, ui_wants, shutdown, tx))?
    };

    let server_handle = {
        let dsu_wants = dsu_wants_device.clone();
        let shutdown = shutdown.clone();
        thread::Builder::new()
            .name("dsu-server".into())
            .spawn(move || -> std::io::Result<()> {
                let mut server = dsu::Server::bind(dsu_port, dsu_expose, dsu_wants, shutdown, rx)?;
                eprintln!(
                    "sc2dsu DSU server listening on {}  (server id 0x{:08X})",
                    server.local_addr()?,
                    server.server_id()
                );
                eprintln!("waiting for client activity before opening controllers ...");
                server.run()
            })?
    };

    match gui_start_minimized {
        Some(start_minimized) => {
            ui::run(shutdown.clone(), ui_wants_device.clone(), start_minimized)
                .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        }
        None => {
            let _ = server_handle.join();
        }
    }

    shutdown.store(true, Ordering::Relaxed);
    let _ = device_handle.join();
    Ok(())
}

fn run_device_thread(
    dsu_wants: Arc<AtomicBool>,
    ui_wants: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    tx: SyncSender<triton::DeviceEvent>,
) {
    let want_device = || dsu_wants.load(Ordering::Relaxed) || ui_wants.load(Ordering::Relaxed);

    let mut api = match HidApi::new() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("controller: HidApi init failed: {e}");
            return;
        }
    };

    let mut controllers = Vec::<ManagedController>::new();
    let mut next_scan = Instant::now();

    while !shutdown.load(Ordering::Relaxed) {
        if !want_device() {
            disconnect_all(&mut controllers, &tx);
            thread::sleep(Duration::from_millis(200));
            next_scan = Instant::now();
            continue;
        }

        if Instant::now() >= next_scan {
            if let Err(e) = api.refresh_devices() {
                eprintln!("controller: refresh_devices failed ({e}); rebuilding HidApi");
                match HidApi::new() {
                    Ok(a) => api = a,
                    Err(e) => {
                        eprintln!("controller: HidApi re-init failed: {e}; backing off");
                        thread::sleep(Duration::from_millis(1000));
                        continue;
                    }
                }
            }
            for info in triton::list_candidates(&api) {
                let path = info.path().to_bytes().to_vec();
                if controllers.iter().any(|controller| controller.path == path) {
                    continue;
                }
                match triton::OpenSlot::open(&api, &info) {
                    Ok(device) => {
                        eprintln!(
                            "controller: opened iface {} (PID {:04X} {})",
                            device.interface_number,
                            device.product_id,
                            triton::pid_label(device.product_id),
                        );
                        controllers.push(ManagedController {
                            path,
                            device,
                            dsu_slot: None,
                            last_sample_at: Instant::now(),
                            consecutive_errors: 0,
                            last_imu_timestamp: None,
                            stale_samples: 0,
                        });
                    }
                    Err(e) => eprintln!(
                        "controller: open iface {} (PID {:04X}) failed: {e}",
                        info.interface_number(),
                        info.product_id()
                    ),
                }
            }
            next_scan = Instant::now() + Duration::from_secs(1);
        }

        if stats::RECALIBRATE_REQUEST.swap(false, Ordering::Relaxed) {
            for controller in &mut controllers {
                controller.device.recalibrate();
            }
        }

        poll_controllers(&mut controllers, &tx);
        thread::sleep(Duration::from_millis(2));
    }

    disconnect_all(&mut controllers, &tx);
}

struct ManagedController {
    path: Vec<u8>,
    device: triton::OpenSlot,
    dsu_slot: Option<u8>,
    last_sample_at: Instant,
    consecutive_errors: u32,
    last_imu_timestamp: Option<u32>,
    stale_samples: u32,
}

fn poll_controllers(
    controllers: &mut Vec<ManagedController>,
    tx: &SyncSender<triton::DeviceEvent>,
) {
    const SILENCE_REOPEN_MS: u128 = 2000;
    const STALE_THRESHOLD: u32 = 100;

    let mut occupied = [false; triton::MAX_CONTROLLERS];
    for controller in controllers.iter() {
        if let Some(slot) = controller.dsu_slot {
            occupied[usize::from(slot)] = true;
        }
    }

    let mut index = 0;
    while index < controllers.len() {
        let mut remove = false;
        match controllers[index].device.read_one(0) {
            Ok(Some(sample)) => {
                controllers[index].consecutive_errors = 0;
                controllers[index].last_sample_at = Instant::now();
                let fresh_sample =
                    controllers[index].last_imu_timestamp != Some(sample.imu.timestamp_us);
                if !fresh_sample {
                    controllers[index].stale_samples += 1;
                    if controllers[index].stale_samples >= STALE_THRESHOLD {
                        eprintln!(
                            "controller: IMU timestamp frozen for {STALE_THRESHOLD} samples; reopening interface"
                        );
                        remove = true;
                    }
                } else {
                    controllers[index].last_imu_timestamp = Some(sample.imu.timestamp_us);
                    controllers[index].stale_samples = 0;
                }
                if fresh_sample {
                    let dsu_slot = match controllers[index].dsu_slot {
                        Some(slot) => Some(slot),
                        None => {
                            let available = occupied
                                .iter()
                                .position(|used| !used)
                                .map(|slot| slot as u8);
                            if let Some(slot) = available {
                                occupied[usize::from(slot)] = true;
                                controllers[index].dsu_slot = Some(slot);
                                eprintln!(
                                    "controller: assigned PID {:04X} iface {} to DSU slot {}",
                                    controllers[index].device.product_id,
                                    controllers[index].device.interface_number,
                                    slot
                                );
                            }
                            available
                        }
                    };
                    if let Some(slot) = dsu_slot {
                        let _ = tx.try_send(triton::DeviceEvent::Sample {
                            slot,
                            state: sample,
                        });
                    }
                }
            }
            Ok(None) => {
                controllers[index].consecutive_errors = 0;
                if controllers[index].last_sample_at.elapsed().as_millis() >= SILENCE_REOPEN_MS {
                    remove = true;
                }
            }
            Err(e) => {
                controllers[index].consecutive_errors += 1;
                if controllers[index].consecutive_errors >= 5 {
                    eprintln!("controller: 5 consecutive read errors ({e}); reopening interface");
                    remove = true;
                }
            }
        }

        if remove {
            let controller = controllers.remove(index);
            if let Some(slot) = controller.dsu_slot {
                occupied[usize::from(slot)] = false;
                let _ = tx.send(triton::DeviceEvent::Disconnected { slot });
                eprintln!("controller: DSU slot {slot} disconnected");
            }
        } else {
            index += 1;
        }
    }
}

fn disconnect_all(controllers: &mut Vec<ManagedController>, tx: &SyncSender<triton::DeviceEvent>) {
    for controller in controllers.drain(..) {
        if let Some(slot) = controller.dsu_slot {
            let _ = tx.send(triton::DeviceEvent::Disconnected { slot });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Mode {
        parse_args_from(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn parse_args_defaults_to_visible_gui() {
        assert_eq!(
            parse(&[]),
            Mode::Gui {
                start_minimized: false
            }
        );
        assert_eq!(
            parse(&["some-positional-arg"]),
            Mode::Gui {
                start_minimized: false
            }
        );
    }

    #[test]
    fn parse_args_recognizes_each_mode() {
        assert_eq!(parse(&["--probe"]), Mode::Probe);
        assert_eq!(parse(&["-p"]), Mode::Probe);
        assert_eq!(parse(&["--headless"]), Mode::Headless);
        assert_eq!(parse(&["-H"]), Mode::Headless);
        assert_eq!(
            parse(&["--gui"]),
            Mode::Gui {
                start_minimized: false
            }
        );
        assert_eq!(
            parse(&["--tray"]),
            Mode::Gui {
                start_minimized: true
            }
        );
        assert_eq!(
            parse(&["--minimized"]),
            Mode::Gui {
                start_minimized: true
            }
        );
    }

    #[test]
    fn parse_args_uses_first_recognized_flag() {
        assert_eq!(
            parse(&["--gui", "--probe"]),
            Mode::Gui {
                start_minimized: false
            }
        );
    }
}
