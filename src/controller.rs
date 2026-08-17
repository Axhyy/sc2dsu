// Device-agnostic controller state shared by every HID backend (Valve's
// Steam/Triton protocol in `triton`, Flydigi's protocol in `flydigi`).

pub const MAX_CONTROLLERS: usize = 4;

#[derive(Debug, Clone, Copy)]
pub struct ImuSample {
    pub timestamp_us: u32,
    pub accel_g: [f32; 3],
    pub gyro_dps: [f32; 3],
}

#[allow(dead_code)]
pub mod button {
    pub const A: u32 = 0x0000_0001;
    pub const B: u32 = 0x0000_0002;
    pub const X: u32 = 0x0000_0004;
    pub const Y: u32 = 0x0000_0008;
    pub const QAM: u32 = 0x0000_0010;
    pub const R3: u32 = 0x0000_0020;
    pub const VIEW: u32 = 0x0000_0040;
    pub const R4: u32 = 0x0000_0080;
    pub const R5: u32 = 0x0000_0100;
    pub const R: u32 = 0x0000_0200;
    pub const DPAD_DOWN: u32 = 0x0000_0400;
    pub const DPAD_RIGHT: u32 = 0x0000_0800;
    pub const DPAD_LEFT: u32 = 0x0000_1000;
    pub const DPAD_UP: u32 = 0x0000_2000;
    pub const MENU: u32 = 0x0000_4000;
    pub const L3: u32 = 0x0000_8000;
    pub const STEAM: u32 = 0x0001_0000;
    pub const L4: u32 = 0x0002_0000;
    pub const L5: u32 = 0x0004_0000;
    pub const L: u32 = 0x0008_0000;
}

#[derive(Debug, Clone, Copy)]
pub struct ControllerState {
    pub buttons: u32,
    pub trigger_left: u16,
    pub trigger_right: u16,
    pub left_stick: [i16; 2],
    pub right_stick: [i16; 2],
    pub imu: ImuSample,
}

#[derive(Debug, Clone, Copy)]
pub enum DeviceEvent {
    Sample { slot: u8, state: ControllerState },
    Disconnected { slot: u8 },
}
