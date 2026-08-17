// Unifies the two HID backends (Valve's Steam/Triton protocol and Flydigi's
// protocol) behind one type so the device-scanning thread and the probe tool
// don't need to know which vendor a candidate interface belongs to.

use crate::controller::ControllerState;
use crate::{flydigi, triton};
use hidapi::{DeviceInfo, HidApi};

pub fn list_candidates(api: &HidApi) -> Vec<DeviceInfo> {
    let mut candidates = triton::list_candidates(api);
    candidates.extend(flydigi::list_candidates(api));
    candidates
}

pub fn pid_label(info: &DeviceInfo) -> &'static str {
    match info.vendor_id() {
        triton::VID_VALVE => triton::pid_label(info.product_id()),
        flydigi::VID_FLYDIGI_V2 => flydigi::pid_label(info.product_id()),
        _ => "?",
    }
}

pub enum OpenSlot {
    Valve(Box<triton::OpenSlot>),
    Flydigi(Box<flydigi::OpenSlot>),
}

impl OpenSlot {
    pub fn open(api: &HidApi, info: &DeviceInfo) -> Result<Self, String> {
        match info.vendor_id() {
            triton::VID_VALVE => {
                triton::OpenSlot::open(api, info).map(|s| OpenSlot::Valve(Box::new(s)))
            }
            flydigi::VID_FLYDIGI_V2 => {
                flydigi::OpenSlot::open(api, info).map(|s| OpenSlot::Flydigi(Box::new(s)))
            }
            vid => Err(format!("unsupported vendor {vid:04X}")),
        }
    }

    pub fn read_one(&mut self, timeout_ms: i32) -> Result<Option<ControllerState>, String> {
        match self {
            OpenSlot::Valve(s) => s.read_one(timeout_ms),
            OpenSlot::Flydigi(s) => s.read_one(timeout_ms),
        }
    }

    pub fn recalibrate(&mut self) {
        match self {
            OpenSlot::Valve(s) => s.recalibrate(),
            OpenSlot::Flydigi(s) => s.recalibrate(),
        }
    }

    pub fn interface_number(&self) -> i32 {
        match self {
            OpenSlot::Valve(s) => s.interface_number,
            OpenSlot::Flydigi(s) => s.interface_number,
        }
    }

    pub fn product_id(&self) -> u16 {
        match self {
            OpenSlot::Valve(s) => s.product_id,
            OpenSlot::Flydigi(s) => s.product_id,
        }
    }

    pub fn input_reports_seen(&self) -> u32 {
        match self {
            OpenSlot::Valve(s) => s.input_reports_seen,
            OpenSlot::Flydigi(s) => s.input_reports_seen,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            OpenSlot::Valve(s) => triton::pid_label(s.product_id),
            OpenSlot::Flydigi(s) => flydigi::pid_label(s.product_id),
        }
    }
}
