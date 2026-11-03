//! The service half of the GPU fan curve. Changing a fan needs administrator
//! rights, so this runs in `ohi-sensor` while the widget only publishes the curve.
//!
//! The rule throughout: when anything is in doubt, hand the fans back to the
//! card's own automatic control, which needs nothing from this program.

use crate::sensor_shared::{
    decode_curve, encode_status, lease_is_fresh, CurvePoint, FanController, FanState, FanStatus, FAN_MAPPING_NAME, FAN_MAPPING_SIZE,
    FAN_WORD_CURVE, FAN_WORD_LEASE, FAN_WORD_MIN_SPEED, FAN_WORD_STATUS,
};
use crate::{create_mapping, wide, Handle};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{FreeLibrary, HANDLE, HMODULE, WAIT_OBJECT_0};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::WaitForSingleObject;

const POLL_MS: u32 = 1_000;
/// Machines without an NVIDIA GPU should not pay for a retry every second.
const REOPEN_AFTER_POLLS: u32 = 10;
const NVML_TEMPERATURE_GPU: u32 = 0;
const NVML_FAN_POLICY_MANUAL: u32 = 1;

type Device = *mut c_void;
type RawProc = unsafe extern "system" fn() -> isize;
type InitFn = unsafe extern "system" fn() -> u32;
type DeviceByIndexFn = unsafe extern "system" fn(u32, *mut Device) -> u32;
type CountFn = unsafe extern "system" fn(Device, *mut u32) -> u32;
type MinMaxFn = unsafe extern "system" fn(Device, *mut u32, *mut u32) -> u32;
type ReadFn = unsafe extern "system" fn(Device, u32, *mut u32) -> u32;
type SetSpeedFn = unsafe extern "system" fn(Device, u32, u32) -> u32;
type SetDefaultFn = unsafe extern "system" fn(Device, u32) -> u32;

/// NVML from System32. Only device 0, the same one the widget shows.
struct Gpu {
    module: HMODULE,
    device: Device,
    fans: u32,
    min_speed: u8,
    max_speed: u8,
    shutdown: InitFn,
    temperature: ReadFn,
    speed: ReadFn,
    target_speed: ReadFn,
    policy: ReadFn,
    set_speed: SetSpeedFn,
    set_default: SetDefaultFn,
}

impl Gpu {
    fn open() -> Option<Gpu> {
        unsafe {
            let name = wide("nvml.dll");
            let module = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
            if module.is_null() {
                return None;
            }
            let gpu = Self::bind(module);
            if gpu.is_none() {
                FreeLibrary(module);
            }
            gpu
        }
    }

    unsafe fn bind(module: HMODULE) -> Option<Gpu> {
        macro_rules! symbol {
            ($name:literal, $signature:ty) => {
                std::mem::transmute::<RawProc, $signature>(GetProcAddress(module, concat!($name, "\0").as_ptr())?)
            };
        }
        let init = symbol!("nvmlInit_v2", InitFn);
        let shutdown = symbol!("nvmlShutdown", InitFn);
        let by_index = symbol!("nvmlDeviceGetHandleByIndex_v2", DeviceByIndexFn);
        let fan_count = symbol!("nvmlDeviceGetNumFans", CountFn);
        let min_max = symbol!("nvmlDeviceGetMinMaxFanSpeed", MinMaxFn);
        let temperature = symbol!("nvmlDeviceGetTemperature", ReadFn);
        let speed = symbol!("nvmlDeviceGetFanSpeed_v2", ReadFn);
        let target_speed = symbol!("nvmlDeviceGetTargetFanSpeed", ReadFn);
        let policy = symbol!("nvmlDeviceGetFanControlPolicy_v2", ReadFn);
        let set_speed = symbol!("nvmlDeviceSetFanSpeed_v2", SetSpeedFn);
        let set_default = symbol!("nvmlDeviceSetDefaultFanSpeed_v2", SetDefaultFn);
        if init() != 0 {
            return None;
        }
        let (mut device, mut fans, mut min, mut max) = (std::ptr::null_mut(), 0, 0, 0);
        if by_index(0, &mut device) != 0
            || fan_count(device, &mut fans) != 0
            || fans == 0
            || min_max(device, &mut min, &mut max) != 0
            || max == 0
        {
            shutdown();
            return None;
        }
        Some(Gpu {
            module,
            device,
            fans,
            min_speed: min.min(100) as u8,
            max_speed: max.min(100) as u8,
            shutdown,
            temperature,
            speed,
            target_speed,
            policy,
            set_speed,
            set_default,
        })
    }

    fn temperature(&self) -> Option<u8> {
        let mut value = 0;
        unsafe { ((self.temperature)(self.device, NVML_TEMPERATURE_GPU, &mut value) == 0).then_some(value.min(255) as u8) }
    }

    fn speed(&self) -> u8 {
        let mut value = 0;
        unsafe { (self.speed)(self.device, 0, &mut value) };
        value.min(255) as u8
    }

    /// Tries every fan even after a failure: the ones that accept must not stay in
    /// manual control because another one refused.
    fn hand_back(&self) -> bool {
        let mut all_accepted = true;
        for fan in 0..self.fans {
            all_accepted &= unsafe { (self.set_default)(self.device, fan) } == 0;
        }
        all_accepted
    }

    /// Written only when different. Checking every time also restores the curve
    /// after anything that resets the card, such as resuming from sleep.
    fn hold(&self, percent: u8) -> bool {
        let percent = u32::from(percent.clamp(self.min_speed, self.max_speed));
        (0..self.fans).all(|fan| unsafe {
            let (mut policy, mut target) = (u32::MAX, u32::MAX);
            let holding = (self.policy)(self.device, fan, &mut policy) == 0
                && (self.target_speed)(self.device, fan, &mut target) == 0
                && policy == NVML_FAN_POLICY_MANUAL
                && target == percent;
            holding || (self.set_speed)(self.device, fan, percent) == 0
        })
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
            FreeLibrary(self.module);
        }
    }
}

struct Channel {
    _mapping: Handle,
    words: *const AtomicU64,
}

impl Channel {
    fn create() -> Result<Channel, u32> {
        // SYSTEM has full access, signed-in users can read, and interactive users
        // can also write the curve.
        let (mapping, address) = create_mapping(FAN_MAPPING_NAME, FAN_MAPPING_SIZE, "D:P(A;;GA;;;SY)(A;;GR;;;AU)(A;;GRGW;;;IU)")?;
        Ok(Channel { _mapping: mapping, words: address.cast() })
    }

    fn word(&self, index: usize) -> &AtomicU64 {
        debug_assert!(index < FAN_MAPPING_SIZE as usize / 8);
        unsafe { &*self.words.add(index) }
    }

    fn curve(&self, now_ms: u64) -> Option<Vec<CurvePoint>> {
        if !lease_is_fresh(self.word(FAN_WORD_LEASE).load(Ordering::Acquire), now_ms) {
            return None;
        }
        let first = self.word(FAN_WORD_CURVE).load(Ordering::Acquire);
        let second = self.word(FAN_WORD_CURVE + 1).load(Ordering::Acquire);
        decode_curve([first, second])
    }

    fn publish(&self, status: FanStatus, now_ms: u64) {
        self.word(FAN_WORD_STATUS).store(encode_status(status, now_ms), Ordering::Release);
    }
}

/// One control cycle. `None` means the GPU failed and must be reopened.
fn step(gpu: &Gpu, controller: &mut FanController, manual: &mut bool, curve: Option<&[CurvePoint]>) -> Option<FanStatus> {
    let temp = gpu.temperature()?;
    let percent = controller.step(curve, temp);
    if percent == 0 {
        if *manual && !gpu.hand_back() {
            return None;
        }
        *manual = false;
    } else {
        *manual = true;
        if !gpu.hold(percent) {
            return None;
        }
    }
    let state = match (percent, controller.is_emergency()) {
        (_, true) => FanState::Emergency,
        (0, _) if curve.is_none() => FanState::Automatic,
        _ => FanState::Curve,
    };
    Some(FanStatus { state, temp, speed: gpu.speed() })
}

/// Runs until `stop` is signaled, and always leaves the card in automatic control.
pub fn run(stop: HANDLE) -> Result<(), u32> {
    let channel = Channel::create()?;
    let mut gpu: Option<Gpu> = None;
    let mut controller = FanController::default();
    let mut manual = false;
    let mut wait_polls = 0;
    loop {
        if gpu.is_none() && wait_polls == 0 {
            gpu = Gpu::open();
            wait_polls = REOPEN_AFTER_POLLS;
            controller = FanController::default();
            manual = false;
            if let Some(opened) = &gpu {
                // A previous run may have crashed while holding the fans.
                opened.hand_back();
                channel.word(FAN_WORD_MIN_SPEED).store(u64::from(opened.min_speed), Ordering::Release);
            }
        }
        wait_polls = wait_polls.saturating_sub(1);

        let now = unsafe { GetTickCount64() };
        let curve = channel.curve(now);
        let status = gpu.as_ref().and_then(|active| step(active, &mut controller, &mut manual, curve.as_deref()));
        match status {
            Some(status) => channel.publish(status, now),
            None => {
                if let Some(failed) = gpu.take() {
                    failed.hand_back();
                }
                channel.publish(FanStatus { state: FanState::Failed, temp: 0, speed: 0 }, now);
            }
        }

        if unsafe { WaitForSingleObject(stop, POLL_MS) } == WAIT_OBJECT_0 {
            if let Some(active) = &gpu {
                active.hand_back();
            }
            channel.word(FAN_WORD_STATUS).store(0, Ordering::Release);
            return Ok(());
        }
    }
}
