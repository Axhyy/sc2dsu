use crate::{autostart, config, stats};
use eframe::egui::{self, Color32, Pos2, Stroke, Vec2};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const AXIS_LABELS: [&str; 3] = ["raw X", "raw Y", "raw Z"];

pub struct App {
    shutdown: Arc<AtomicBool>,
    ui_wants_device: Arc<AtomicBool>,
    cfg: config::Config,
    port: String,
    note: String,
    tray: Option<TrayIcon>,
    tray_show_id: Option<tray_icon::menu::MenuId>,
    tray_quit_id: Option<tray_icon::menu::MenuId>,
    visible: bool,
    confirm_defaults: bool,
}

impl App {
    fn new(
        shutdown: Arc<AtomicBool>,
        ui_wants_device: Arc<AtomicBool>,
        start_minimized: bool,
    ) -> Self {
        let cfg = config::snapshot();
        let visible = !(cfg.start_minimized || start_minimized);
        ui_wants_device.store(visible, Ordering::Relaxed);
        let (tray, tray_show_id, tray_quit_id) = make_tray().unwrap_or_else(|e| {
            eprintln!("ui: system tray unavailable: {e}");
            (None, None, None)
        });
        Self {
            shutdown,
            ui_wants_device,
            port: cfg.port.to_string(),
            cfg,
            note: String::new(),
            tray,
            tray_show_id,
            tray_quit_id,
            visible,
            confirm_defaults: false,
        }
    }

    fn set_visible(&mut self, ctx: &egui::Context, visible: bool) {
        self.visible = visible;
        self.ui_wants_device.store(visible, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(visible));
        if visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn save(&mut self, note: impl Into<String>) {
        match config::update_and_save(self.cfg.clone()) {
            Ok(()) => self.note = note.into(),
            Err(e) => self.note = format!("save failed: {e}"),
        }
    }

    fn axis_row(ui: &mut egui::Ui, id: &str, label: &str, axis: &mut config::Axis) -> bool {
        let mut changed = false;
        ui.label(label);
        egui::ComboBox::from_id_salt(id)
            .selected_text(AXIS_LABELS[(axis.source as usize).min(2)])
            .show_ui(ui, |ui| {
                for (source, text) in AXIS_LABELS.iter().enumerate() {
                    changed |= ui
                        .selectable_value(&mut axis.source, source as u8, *text)
                        .changed();
                }
            });
        changed |= ui.checkbox(&mut axis.invert, "invert").changed();
        ui.end_row();
        changed
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.columns(2, |columns| {
            columns[0].group(|ui| {
                ui.heading("Gyro axis mapping");
                egui::Grid::new("gyro-map").show(ui, |ui| {
                    changed |= Self::axis_row(ui, "gx", "DSU X (pitch)", &mut self.cfg.gyro.x);
                    changed |= Self::axis_row(ui, "gy", "DSU Y (yaw)", &mut self.cfg.gyro.y);
                    changed |= Self::axis_row(ui, "gz", "DSU Z (roll)", &mut self.cfg.gyro.z);
                });
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut self.cfg.gyro_sensitivity,
                            config::GYRO_SENSITIVITY_MIN..=config::GYRO_SENSITIVITY_MAX,
                        )
                        .text("Sensitivity")
                        .fixed_decimals(2),
                    )
                    .changed();
                changed |= ui
                    .checkbox(&mut self.cfg.auto_calibrate, "Auto-calibrate bias")
                    .changed();
            });
            columns[1].group(|ui| {
                ui.horizontal(|ui| {
                    ui.heading("Accel axis mapping");
                    if ui.button("Copy from gyro").clicked() {
                        self.cfg.accel = self.cfg.gyro;
                        changed = true;
                    }
                });
                egui::Grid::new("accel-map").show(ui, |ui| {
                    changed |= Self::axis_row(ui, "ax", "DSU X", &mut self.cfg.accel.x);
                    changed |= Self::axis_row(ui, "ay", "DSU Y", &mut self.cfg.accel.y);
                    changed |= Self::axis_row(ui, "az", "DSU Z", &mut self.cfg.accel.z);
                });
            });
        });
        if changed {
            self.save("settings saved.");
        }
    }

    fn system_settings(&mut self, ui: &mut egui::Ui) {
        let mut save = false;
        ui.group(|ui| {
            ui.heading("System");
            ui.horizontal_wrapped(|ui| {
                ui.label("UDP port (next launch):");
                if ui
                    .add(egui::TextEdit::singleline(&mut self.port).desired_width(75.0))
                    .lost_focus()
                {
                    match self.port.parse::<u16>() {
                        Ok(port) => {
                            self.cfg.port = port;
                            save = true;
                        }
                        Err(_) => self.note = "port must be a number from 0 to 65535.".into(),
                    }
                }
                save |= ui
                    .checkbox(&mut self.cfg.expose_to_network, "Open to network")
                    .changed();
                save |= ui
                    .checkbox(&mut self.cfg.start_minimized, "Start minimized to tray")
                    .changed();
                save |= ui
                    .checkbox(&mut self.cfg.close_to_tray, "Hide to tray on close")
                    .changed();
                let mut enabled = autostart::is_enabled();
                if ui.checkbox(&mut enabled, "Start with system").changed() {
                    let result = if enabled {
                        autostart::enable()
                    } else {
                        autostart::disable()
                    };
                    self.note = match result {
                        Ok(()) => format!(
                            "autostart {}.",
                            if enabled { "enabled" } else { "disabled" }
                        ),
                        Err(e) => format!("autostart change failed: {e}"),
                    };
                }
            });
        });
        if save {
            self.save("system settings saved.");
        }
    }

    fn status(&self, ui: &mut egui::Ui) {
        let s = stats::snapshot();
        let host = config::bind_host(self.cfg.expose_to_network);
        ui.group(|ui| {
            ui.heading("Status");
            egui::Grid::new("status").num_columns(2).show(ui, |ui| {
                ui.label("Listening on:");
                ui.monospace(if s.server.bound_port == 0 {
                    "binding…".into()
                } else {
                    format!("{host}:{}", s.server.bound_port)
                });
                ui.end_row();
                ui.label("Server id:");
                ui.monospace(format!("0x{:08X}", s.server.server_id));
                ui.end_row();
                ui.label("Controllers / subscribers:");
                let device = if s.server.device_active {
                    "awake"
                } else {
                    "idle"
                };
                let calibration = if !s.calibration.active {
                    "calibration off".into()
                } else if s.calibration.steady {
                    format!(
                        "calibration locked {:.0}%",
                        s.calibration.confidence * 100.0
                    )
                } else {
                    "calibrating…".into()
                };
                ui.monospace(format!(
                    "{} / {} ({device}, {calibration})",
                    s.server.controllers, s.server.subscribers
                ));
                ui.end_row();
                ui.label("IMU / packets / requests:");
                ui.monospace(format!(
                    "{:.1} Hz / {:.1} s⁻¹ / {:.1} s⁻¹",
                    s.server.samples_per_sec, s.server.packets_per_sec, s.server.requests_per_sec
                ));
                ui.end_row();
                ui.label("Gyro (deg/s):");
                ui.monospace(format!(
                    "{:+8.1} {:+8.1} {:+8.1}",
                    s.motion.last_gyro_dps[0], s.motion.last_gyro_dps[1], s.motion.last_gyro_dps[2]
                ));
                ui.end_row();
                ui.label("Accel (g):");
                ui.monospace(format!(
                    "{:+6.3} {:+6.3} {:+6.3}",
                    s.motion.last_accel_g[0], s.motion.last_accel_g[1], s.motion.last_accel_g[2]
                ));
                ui.end_row();
            });
        });
    }

    fn visualization(&self, ui: &mut egui::Ui) {
        let (response, painter) =
            ui.allocate_painter(Vec2::new(ui.available_width(), 180.0), egui::Sense::hover());
        let rect = response.rect;
        painter.rect_filled(rect, 4.0, Color32::from_rgb(32, 32, 32));
        let q = stats::snapshot().motion.orientation;
        let project = |v: [f32; 3]| {
            let r = quat_rotate(q, v);
            Pos2::new(rect.center().x + r[0] * 55.0, rect.center().y - r[1] * 55.0)
        };
        const V: [[f32; 3]; 8] = [
            [-1.0, -0.4, -1.0],
            [1.0, -0.4, -1.0],
            [1.0, 0.4, -1.0],
            [-1.0, 0.4, -1.0],
            [-1.0, -0.4, 1.0],
            [1.0, -0.4, 1.0],
            [1.0, 0.4, 1.0],
            [-1.0, 0.4, 1.0],
        ];
        const E: [(usize, usize); 12] = [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        for (a, b) in E {
            painter.line_segment(
                [project(V[a]), project(V[b])],
                Stroke::new(2.0, Color32::from_rgb(128, 224, 96)),
            );
        }
        for (axis, color) in [
            ([1.6, 0.0, 0.0], Color32::RED),
            ([0.0, 1.6, 0.0], Color32::GREEN),
            ([0.0, 0.0, 1.6], Color32::BLUE),
        ] {
            painter.line_segment([project([0.0; 3]), project(axis)], Stroke::new(3.0, color));
        }
    }

    fn handle_tray(&mut self, ctx: &egui::Context) {
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                self.set_visible(ctx, !self.visible);
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if Some(&event.id) == self.tray_show_id.as_ref() {
                self.set_visible(ctx, true);
            }
            if Some(&event.id) == self.tray_quit_id.as_ref() {
                self.quit(ctx);
            }
        }
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.shutdown.store(true, Ordering::Relaxed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_tray(ctx);
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.cfg.close_to_tray && self.tray.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.set_visible(ctx, false);
            } else {
                self.shutdown.store(true, Ordering::Relaxed);
            }
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            self.status(ui);
            ui.add_space(6.0);
            self.settings(ui);
            ui.add_space(6.0);
            self.system_settings(ui);
            ui.add_space(6.0);
            self.visualization(ui);
            ui.horizontal(|ui| {
                if ui.button("Hide to tray").clicked() {
                    self.set_visible(ctx, false);
                }
                if ui.button("Recalibrate").clicked() {
                    stats::RECENTER_REQUEST.store(true, Ordering::Relaxed);
                    stats::RECALIBRATE_REQUEST.store(true, Ordering::Relaxed);
                    self.note = "recalibrating gyro.".into();
                }
                if ui
                    .button(if self.confirm_defaults {
                        "Confirm restore"
                    } else {
                        "Restore defaults"
                    })
                    .clicked()
                {
                    if self.confirm_defaults {
                        self.cfg = config::Config::DEFAULT;
                        self.port = self.cfg.port.to_string();
                        self.save("restored defaults.");
                    }
                    self.confirm_defaults = !self.confirm_defaults;
                }
                if ui.button("Quit").clicked() {
                    self.quit(ctx);
                }
                ui.label(&self.note);
            });
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

fn make_tray() -> Result<
    (
        Option<TrayIcon>,
        Option<tray_icon::menu::MenuId>,
        Option<tray_icon::menu::MenuId>,
    ),
    String,
> {
    let menu = Menu::new();
    let show = MenuItem::new("Show settings", true, None);
    let quit = MenuItem::new("Quit", true, None);
    menu.append(&show).map_err(|e| e.to_string())?;
    menu.append(&quit).map_err(|e| e.to_string())?;
    let show_id = show.id().clone();
    let quit_id = quit.id().clone();
    let icon = Icon::from_rgba(vec![60, 130, 220, 255].repeat(16 * 16), 16, 16)
        .map_err(|e| e.to_string())?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("SC2DSU")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
        .map_err(|e| e.to_string())?;
    Ok((Some(tray), Some(show_id), Some(quit_id)))
}

pub fn run(
    shutdown: Arc<AtomicBool>,
    ui_wants_device: Arc<AtomicBool>,
    start_minimized: bool,
) -> Result<(), String> {
    let cfg = config::snapshot();
    let visible = !(cfg.start_minimized || start_minimized);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 850.0])
            .with_visible(visible),
        ..Default::default()
    };
    eframe::run_native(
        "SC2DSU — Steam Controller gyro to Cemuhook",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(App::new(
                shutdown,
                ui_wants_device,
                start_minimized,
            )))
        }),
    )
    .map_err(|e| e.to_string())
}

fn quat_rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let (w, x, y, z) = (q[0], q[1], q[2], q[3]);
    let qx = [x, y, z];
    let c1 = cross(qx, v);
    let t = [c1[0] + w * v[0], c1[1] + w * v[1], c1[2] + w * v[2]];
    let c2 = cross(qx, t);
    [v[0] + 2.0 * c2[0], v[1] + 2.0 * c2[1], v[2] + 2.0 * c2[2]]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn approx_eq(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }
    #[test]
    fn quat_rotate_identity_is_noop() {
        let v = [0.3, -1.2, 4.0];
        assert!(approx_eq(quat_rotate([1.0, 0.0, 0.0, 0.0], v), v));
    }
    #[test]
    fn quat_rotate_90deg_about_z_maps_x_to_y() {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        assert!(approx_eq(
            quat_rotate([s, 0.0, 0.0, s], [1.0, 0.0, 0.0]),
            [0.0, 1.0, 0.0]
        ));
    }
    #[test]
    fn cross_of_basis_vectors() {
        assert_eq!(cross([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]);
    }
}
