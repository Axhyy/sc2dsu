// Flydigi "V2" HIDAPI protocol, covering the Vader 5 Pro. Ported from SDL3's
// SDL_hidapi_flydigi.c and src/joystick/usb_ids.h (MIT). The Vader 5 Pro
// speaks unnumbered 32-byte reports on its single vendor-usage HID
// interface: a magic-prefixed command/response channel used to acquire
// control of the pad away from the Flydigi Space Station app, plus a
// streamed input report carrying buttons, sticks, triggers, and the IMU.
//
// The pad must have "Allow third-party apps to take over mappings" enabled
// in the Flydigi Space Station app, or the acquire request below is refused
// and no input reports are sent.

use crate::config;
use crate::controller::{ControllerState, ImuSample, button};
use crate::gyro_calibration::GyroCalibration;
use crate::stats;
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::time::{Duration, Instant};

pub const VID_FLYDIGI_V2: u16 = 0x37D7;
pub const PID_FLYDIGI_VADER: u16 = 0x2401;
const USAGE_PAGE_VENDOR: u16 = 0xFFA0;

const PACKET_LEN: usize = 32;
const MAGIC1: u8 = 0x5A;
const MAGIC2: u8 = 0xA5;
const CMD_REPORT_ID: u8 = 0x03;

const CMD_GET_INFO: u8 = 0x01;
const CMD_GET_STATUS: u8 = 0x10;
const CMD_ACQUIRE: u8 = 0x1C;
const CMD_INPUT_REPORT: u8 = 0xEF;

// Vader 5 Pro's minimum supported firmware, from SDL3's InitControllerV2.
const MIN_FIRMWARE_VADER5_PRO: u16 = 0x7141;
const DEVICE_ID_VADER5_PRO: u8 = 130;

// The device auto-releases control after a while; SDL re-acquires every 30s
// and whenever the input stream goes quiet for a moment.
const ACQUIRE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const INPUT_SILENCE_REACQUIRE: Duration = Duration::from_millis(100);

// Vader 5 Pro's observed wired IMU packet rate (SDL3's SENSOR_INTERVAL_VADER5_PRO_RATE_HZ).
const SENSOR_INTERVAL_US: u32 = 1_000_000 / 500;

pub fn is_candidate(info: &DeviceInfo) -> bool {
    info.vendor_id() == VID_FLYDIGI_V2
        && info.product_id() == PID_FLYDIGI_VADER
        && info.usage_page() == USAGE_PAGE_VENDOR
}

pub fn list_candidates(api: &HidApi) -> Vec<DeviceInfo> {
    api.device_list()
        .filter(|d| is_candidate(d))
        .cloned()
        .collect()
}

pub fn pid_label(_pid: u16) -> &'static str {
    "Flydigi Vader 5 Pro"
}

fn be_i16(b: &[u8]) -> i16 {
    i16::from_be_bytes([b[0], b[1]])
}

// Vader 5 Pro's write path uses unnumbered reports: the report-ID byte that
// prefixes every command below must be zeroed before it hits the wire.
fn write_packet(dev: &HidDevice, data: &[u8]) -> Result<(), String> {
    let mut buf = [0u8; PACKET_LEN];
    let n = data.len().min(PACKET_LEN);
    buf[..n].copy_from_slice(&data[..n]);
    if buf[0] == CMD_REPORT_ID {
        buf[0] = 0;
    }
    dev.write(&buf).map_err(|e| format!("write: {e}"))?;
    Ok(())
}

fn send_info_request(dev: &HidDevice) -> Result<(), String> {
    write_packet(dev, &[CMD_REPORT_ID, MAGIC1, MAGIC2, CMD_GET_INFO, 2, 0])
}

fn send_status_request(dev: &HidDevice) -> Result<(), String> {
    write_packet(dev, &[CMD_REPORT_ID, MAGIC1, MAGIC2, CMD_GET_STATUS])
}

fn send_acquire_request(dev: &HidDevice, acquire: bool) -> Result<(), String> {
    let mut cmd = [0u8; PACKET_LEN];
    cmd[0] = CMD_REPORT_ID;
    cmd[1] = MAGIC1;
    cmd[2] = MAGIC2;
    cmd[3] = CMD_ACQUIRE;
    cmd[4] = 23;
    cmd[5] = u8::from(acquire);
    cmd[6..9].copy_from_slice(b"SDL");
    write_packet(dev, &cmd)
}

// Normalizes a raw HID read into the magic-prefixed 31-byte command frame,
// stripping a leading report-ID byte if the backend still exposes one.
fn normalize_frame(buf: &[u8]) -> Option<&[u8]> {
    let frame = if buf.first() == Some(&MAGIC1) {
        buf
    } else if buf.len() > 1 {
        &buf[1..]
    } else {
        return None;
    };
    if frame.len() < 31 || frame[0] != MAGIC1 || frame[1] != MAGIC2 {
        return None;
    }
    Some(frame)
}

fn wait_for_reply(
    dev: &HidDevice,
    command: u8,
    timeout_ms: i32,
) -> Result<[u8; PACKET_LEN], String> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    while Instant::now() < deadline {
        let mut buf = [0u8; PACKET_LEN];
        match dev.read_timeout(&mut buf, 20) {
            Ok(0) => continue,
            Ok(_) => {
                if let Some(frame) = normalize_frame(&buf)
                    && frame[2] == command
                {
                    return Ok(buf);
                }
            }
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    Err(format!("no reply to command 0x{command:02X}"))
}

struct DeviceInfoReply {
    device_id: u8,
    firmware_version: u16,
}

fn parse_info_reply(buf: &[u8; PACKET_LEN]) -> Option<DeviceInfoReply> {
    let frame = normalize_frame(buf)?;
    Some(DeviceInfoReply {
        device_id: frame[5],
        firmware_version: u16::from_be_bytes([frame[16], frame[15]]),
    })
}

fn configure(dev: &HidDevice) -> Result<(), String> {
    send_info_request(dev)?;
    let reply = wait_for_reply(dev, CMD_GET_INFO, 500)?;
    let info = parse_info_reply(&reply).ok_or("malformed info reply")?;
    if info.device_id != DEVICE_ID_VADER5_PRO {
        return Err(format!(
            "unrecognized Flydigi device id {} (expected Vader 5 Pro / {DEVICE_ID_VADER5_PRO})",
            info.device_id
        ));
    }
    if info.firmware_version < MIN_FIRMWARE_VADER5_PRO {
        return Err(format!(
            "firmware 0x{:04X} is older than the minimum supported 0x{MIN_FIRMWARE_VADER5_PRO:04X}; update via Flydigi Space Station",
            info.firmware_version
        ));
    }
    send_status_request(dev)?;
    send_acquire_request(dev, true)?;
    Ok(())
}

fn parse_input_report(frame: &[u8]) -> Option<ControllerState> {
    if frame.len() < 29 || frame[2] != CMD_INPUT_REPORT {
        return None;
    }

    let mut buttons = 0u32;
    match frame[11] & 0x0F {
        0x01 => buttons |= button::DPAD_UP,
        0x03 => buttons |= button::DPAD_UP | button::DPAD_RIGHT,
        0x02 => buttons |= button::DPAD_RIGHT,
        0x06 => buttons |= button::DPAD_RIGHT | button::DPAD_DOWN,
        0x04 => buttons |= button::DPAD_DOWN,
        0x0C => buttons |= button::DPAD_DOWN | button::DPAD_LEFT,
        0x08 => buttons |= button::DPAD_LEFT,
        0x09 => buttons |= button::DPAD_LEFT | button::DPAD_UP,
        _ => {}
    }
    if frame[11] & 0x10 != 0 {
        buttons |= button::A;
    }
    if frame[11] & 0x20 != 0 {
        buttons |= button::B;
    }
    if frame[11] & 0x40 != 0 {
        buttons |= button::VIEW;
    }
    if frame[11] & 0x80 != 0 {
        buttons |= button::X;
    }

    if frame[12] & 0x01 != 0 {
        buttons |= button::Y;
    }
    if frame[12] & 0x02 != 0 {
        buttons |= button::MENU;
    }
    if frame[12] & 0x04 != 0 {
        buttons |= button::L;
    }
    if frame[12] & 0x08 != 0 {
        buttons |= button::R;
    }
    if frame[12] & 0x40 != 0 {
        buttons |= button::L3;
    }
    if frame[12] & 0x80 != 0 {
        buttons |= button::R3;
    }

    // M1-M4 (rear paddles) plus, on the Vader 5 Pro, the C/Z buttons and the
    // extra shoulder LM/RM buttons all live in this byte.
    if frame[13] & 0x04 != 0 {
        buttons |= button::L4;
    }
    if frame[13] & 0x08 != 0 {
        buttons |= button::R4;
    }
    if frame[13] & 0x10 != 0 {
        buttons |= button::L5;
    }
    if frame[13] & 0x20 != 0 {
        buttons |= button::R5;
    }

    if frame[14] & 0x08 != 0 {
        buttons |= button::STEAM;
    }

    let left_stick = [be_i16(&frame[3..5]), be_i16(&frame[5..7]).saturating_neg()];
    let right_stick = [be_i16(&frame[7..9]), be_i16(&frame[9..11]).saturating_neg()];

    let trigger = |v: u8| ((u32::from(v) * 257).min(i16::MAX as u32)) as u16;
    let trigger_left = trigger(frame[15]);
    let trigger_right = trigger(frame[16]);

    // Big-endian raw axes, full int16 range mapped to +/-2000 dps and +/-1 g
    // (accelScale = STANDARD_GRAVITY/4096 in SDL3, i.e. 4096 counts per g).
    let to_dps = |v: i16| (f32::from(v) / 32768.0) * 2000.0;
    let to_g = |v: i16| f32::from(v) / 4096.0;

    let gyro_x = be_i16(&frame[17..19]);
    let gyro_z = be_i16(&frame[19..21]);
    let gyro_y = be_i16(&frame[21..23]);
    let gyro_dps = [to_dps(gyro_x), to_dps(gyro_y), to_dps(-gyro_z)];

    let accel_x = be_i16(&frame[23..25]);
    let accel_z = be_i16(&frame[25..27]);
    let accel_y = be_i16(&frame[27..29]);
    let accel_g = [to_g(accel_x), to_g(accel_y), to_g(-accel_z)];

    Some(ControllerState {
        buttons,
        trigger_left,
        trigger_right,
        left_stick,
        right_stick,
        imu: ImuSample {
            timestamp_us: 0,
            accel_g,
            gyro_dps,
        },
    })
}

pub struct OpenSlot {
    dev: HidDevice,
    last_acquire: Instant,
    last_input: Instant,
    sensor_timestamp_us: u32,
    gyro_map: config::AxisMap,
    accel_map: config::AxisMap,
    gyro_sensitivity: f32,
    gyro_cal: GyroCalibration,
    auto_calibrate: bool,
    cfg_generation: u64,
    pub interface_number: i32,
    pub product_id: u16,
    pub input_reports_seen: u32,
}

impl OpenSlot {
    pub fn open(api: &HidApi, info: &DeviceInfo) -> Result<Self, String> {
        let dev = api
            .open_path(info.path())
            .map_err(|e| format!("open: {e}"))?;
        configure(&dev)?;
        let _ = dev.set_blocking_mode(false);
        let cfg_generation = config::generation();
        let cfg = config::snapshot();
        let now = Instant::now();
        Ok(Self {
            dev,
            last_acquire: now,
            last_input: now,
            sensor_timestamp_us: 0,
            gyro_map: cfg.gyro,
            accel_map: cfg.accel,
            gyro_sensitivity: cfg.effective_gyro_sensitivity(),
            gyro_cal: GyroCalibration::new(),
            auto_calibrate: cfg.auto_calibrate,
            cfg_generation,
            interface_number: info.interface_number(),
            product_id: info.product_id(),
            input_reports_seen: 0,
        })
    }

    pub fn read_one(&mut self, timeout_ms: i32) -> Result<Option<ControllerState>, String> {
        let now = Instant::now();
        if now.duration_since(self.last_acquire) >= ACQUIRE_HEARTBEAT_INTERVAL
            || now.duration_since(self.last_input) >= INPUT_SILENCE_REACQUIRE
        {
            let _ = send_acquire_request(&self.dev, true);
            let _ = send_info_request(&self.dev);
            self.last_acquire = now;
        }

        let live_generation = config::generation();
        if live_generation != self.cfg_generation {
            let cfg = config::snapshot();
            self.gyro_map = cfg.gyro;
            self.accel_map = cfg.accel;
            self.gyro_sensitivity = cfg.effective_gyro_sensitivity();
            self.auto_calibrate = cfg.auto_calibrate;
            self.cfg_generation = live_generation;
            self.gyro_cal.reset();
        }

        let mut buf = [0u8; PACKET_LEN];
        match self.dev.read_timeout(&mut buf, timeout_ms) {
            Ok(0) => Ok(None),
            Ok(_) => {
                self.input_reports_seen = self.input_reports_seen.saturating_add(1);
                let Some(frame) = normalize_frame(&buf) else {
                    return Ok(None);
                };
                let Some(mut state) = parse_input_report(frame) else {
                    return Ok(None);
                };
                self.last_input = Instant::now();

                let raw_accel = state.imu.accel_g;
                let raw_gyro = state.imu.gyro_dps;
                self.sensor_timestamp_us =
                    self.sensor_timestamp_us.wrapping_add(SENSOR_INTERVAL_US);
                state.imu.timestamp_us = self.sensor_timestamp_us;
                state.imu.accel_g = [
                    self.accel_map.x.apply(raw_accel),
                    self.accel_map.y.apply(raw_accel),
                    self.accel_map.z.apply(raw_accel),
                ];
                state.imu.gyro_dps = [
                    self.gyro_map.x.apply(raw_gyro),
                    self.gyro_map.y.apply(raw_gyro),
                    self.gyro_map.z.apply(raw_gyro),
                ];

                let dt = SENSOR_INTERVAL_US as f32 / 1_000_000.0;
                if self.auto_calibrate {
                    state.imu.gyro_dps =
                        self.gyro_cal
                            .correct(state.imu.gyro_dps, state.imu.accel_g, dt);
                }
                if self.gyro_sensitivity != 1.0 {
                    for v in &mut state.imu.gyro_dps {
                        *v *= self.gyro_sensitivity;
                    }
                }
                stats::publish_calibration(stats::CalibrationSection {
                    active: self.auto_calibrate,
                    steady: self.gyro_cal.is_steady(),
                    confidence: self.gyro_cal.confidence(),
                });
                Ok(Some(state))
            }
            Err(e) => Err(format!("read: {e}")),
        }
    }

    pub fn recalibrate(&mut self) {
        self.gyro_cal.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_input_frame(
        left: [i16; 2],
        right: [i16; 2],
        triggers: [u8; 2],
        buttons_11_14: [u8; 4],
        gyro_raw: [i16; 3],
        accel_raw: [i16; 3],
    ) -> Vec<u8> {
        let mut f = vec![0u8; 31];
        f[0] = MAGIC1;
        f[1] = MAGIC2;
        f[2] = CMD_INPUT_REPORT;
        f[3..5].copy_from_slice(&left[0].to_be_bytes());
        f[5..7].copy_from_slice(&(-left[1]).to_be_bytes());
        f[7..9].copy_from_slice(&right[0].to_be_bytes());
        f[9..11].copy_from_slice(&(-right[1]).to_be_bytes());
        f[11] = buttons_11_14[0];
        f[12] = buttons_11_14[1];
        f[13] = buttons_11_14[2];
        f[14] = buttons_11_14[3];
        f[15] = triggers[0];
        f[16] = triggers[1];
        f[17..19].copy_from_slice(&gyro_raw[0].to_be_bytes());
        f[19..21].copy_from_slice(&(-gyro_raw[2]).to_be_bytes());
        f[21..23].copy_from_slice(&gyro_raw[1].to_be_bytes());
        f[23..25].copy_from_slice(&accel_raw[0].to_be_bytes());
        f[25..27].copy_from_slice(&(-accel_raw[2]).to_be_bytes());
        f[27..29].copy_from_slice(&accel_raw[1].to_be_bytes());
        f
    }

    #[test]
    fn parse_input_report_rejects_short_or_wrong_command() {
        assert!(parse_input_report(&[0u8; 28]).is_none());
        let mut frame = vec![0u8; 31];
        frame[0] = MAGIC1;
        frame[1] = MAGIC2;
        frame[2] = CMD_GET_INFO;
        assert!(parse_input_report(&frame).is_none());
    }

    #[test]
    fn parse_input_report_decodes_sticks_triggers_and_buttons() {
        let frame = build_input_frame(
            [1000, -2000],
            [3000, 4000],
            [100, 255],
            [0x10 | 0x01, 0x04, 0x04, 0x08],
            [0, 0, 0],
            [0, 0, 0],
        );
        let s = parse_input_report(&frame).unwrap();
        assert_eq!(s.left_stick, [1000, -2000]);
        assert_eq!(s.right_stick, [3000, 4000]);
        assert_eq!(s.trigger_left, 100 * 257);
        assert_eq!(s.trigger_right, i16::MAX as u16);
        assert_eq!(
            s.buttons,
            button::A | button::DPAD_UP | button::L | button::L4 | button::STEAM
        );
    }

    #[test]
    fn parse_input_report_decodes_full_scale_imu() {
        let frame = build_input_frame(
            [0, 0],
            [0, 0],
            [0, 0],
            [0, 0, 0, 0],
            [16384, -16384, 0],
            [4096, -4096, 0],
        );
        let s = parse_input_report(&frame).unwrap();
        assert!((s.imu.gyro_dps[0] - 1000.0).abs() < 1e-2);
        assert!((s.imu.gyro_dps[1] + 1000.0).abs() < 1e-2);
        assert!(s.imu.gyro_dps[2].abs() < 1e-2);
        assert!((s.imu.accel_g[0] - 1.0).abs() < 1e-4);
        assert!((s.imu.accel_g[1] + 1.0).abs() < 1e-4);
        assert!(s.imu.accel_g[2].abs() < 1e-4);
    }

    #[test]
    fn parse_info_reply_reads_device_id_and_firmware() {
        let mut buf = [0u8; PACKET_LEN];
        buf[0] = MAGIC1;
        buf[1] = MAGIC2;
        buf[2] = CMD_GET_INFO;
        buf[5] = DEVICE_ID_VADER5_PRO;
        buf[15] = 0x41;
        buf[16] = 0x71;
        let info = parse_info_reply(&buf).unwrap();
        assert_eq!(info.device_id, DEVICE_ID_VADER5_PRO);
        assert_eq!(info.firmware_version, MIN_FIRMWARE_VADER5_PRO);
    }

    #[test]
    fn normalize_frame_strips_leading_report_id_byte() {
        let mut buf = [0u8; PACKET_LEN];
        buf[0] = 0; // report-ID byte some backends still prepend
        buf[1] = MAGIC1;
        buf[2] = MAGIC2;
        buf[3] = CMD_INPUT_REPORT;
        let frame = normalize_frame(&buf).unwrap();
        assert_eq!(frame[0], MAGIC1);
        assert_eq!(frame[1], MAGIC2);
        assert_eq!(frame[2], CMD_INPUT_REPORT);
    }
}
