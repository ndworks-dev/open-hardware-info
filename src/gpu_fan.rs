//! The unprivileged half of the GPU fan curve: the widget keeps renewing the curve
//! for the `ohi-sensor` service, and the settings window reads what it is doing.

use crate::sensor_shared::{
    decode_status, encode_curve, CurvePoint, FanStatus, FAN_MAPPING_NAME, FAN_MAPPING_SIZE, FAN_WORD_CURVE, FAN_WORD_LEASE,
    FAN_WORD_MIN_SPEED, FAN_WORD_STATUS,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;

/// Well inside the service's lease, so one slow cycle cannot drop the curve.
const RENEW_EVERY: Duration = Duration::from_secs(2);

struct View {
    mapping: HANDLE,
    address: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl View {
    fn open(access: u32) -> Option<View> {
        unsafe {
            let name = crate::wide(FAN_MAPPING_NAME);
            let mapping = OpenFileMappingW(access, 0, name.as_ptr());
            if mapping.is_null() {
                return None;
            }
            let address = MapViewOfFile(mapping, access, 0, 0, FAN_MAPPING_SIZE as usize);
            if address.Value.is_null() {
                CloseHandle(mapping);
                return None;
            }
            Some(View { mapping, address })
        }
    }

    fn word(&self, index: usize) -> &AtomicU64 {
        debug_assert!(index < FAN_MAPPING_SIZE as usize / 8);
        // A mapped view is page-aligned and the mapping holds every word.
        unsafe { &*(self.address.Value as *const AtomicU64).add(index) }
    }
}

// The mapping belongs to the whole process and all access to it is atomic.
unsafe impl Send for View {}

impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.address);
            CloseHandle(self.mapping);
        }
    }
}

/// What the service reports, if it is running a version with fan control.
pub struct ServiceFan {
    pub status: Option<FanStatus>,
    pub min_speed: u8,
}

pub fn read_service() -> Option<ServiceFan> {
    let view = View::open(FILE_MAP_READ)?;
    let now = unsafe { GetTickCount64() };
    let status = decode_status(view.word(FAN_WORD_STATUS).load(Ordering::Acquire), now);
    let min_speed = view.word(FAN_WORD_MIN_SPEED).load(Ordering::Acquire).min(100) as u8;
    Some(ServiceFan { status, min_speed })
}

#[derive(Default)]
struct Shared {
    curve: Option<Vec<CurvePoint>>,
    view: Option<View>,
}

impl Shared {
    /// Called with the lock held, so a renewal cannot undo a release. The service
    /// may be installed later, so a missing view is retried. An open view keeps the
    /// mapping alive, so a restarted service finds this same one.
    fn publish(&mut self) {
        if self.view.is_none() && self.curve.is_some() {
            self.view = View::open(FILE_MAP_READ | FILE_MAP_WRITE);
        }
        let Some(view) = &self.view else { return };
        match &self.curve {
            Some(points) => {
                let [first, second] = encode_curve(points);
                view.word(FAN_WORD_CURVE).store(first, Ordering::Release);
                view.word(FAN_WORD_CURVE + 1).store(second, Ordering::Release);
                view.word(FAN_WORD_LEASE).store(unsafe { GetTickCount64() }, Ordering::Release);
            }
            None => view.word(FAN_WORD_LEASE).store(0, Ordering::Release),
        }
    }
}

pub struct FanLink(Arc<Mutex<Shared>>);

impl FanLink {
    /// A separate thread, because the sampling interval can reach a minute and the
    /// main thread shares its input queue with Explorer.
    pub fn spawn(curve: Option<Vec<CurvePoint>>) -> FanLink {
        let shared = Arc::new(Mutex::new(Shared { curve, view: None }));
        let renewing = Arc::clone(&shared);
        std::thread::spawn(move || loop {
            if let Ok(mut shared) = renewing.lock() {
                shared.publish();
            }
            std::thread::sleep(RENEW_EVERY);
        });
        FanLink(shared)
    }

    pub fn set_curve(&self, curve: Option<Vec<CurvePoint>>) {
        if let Ok(mut shared) = self.0.lock() {
            shared.curve = curve;
            shared.publish();
        }
    }

    /// Ends the lease right away rather than waiting for it to expire.
    pub fn release(&self) {
        self.set_curve(None);
    }
}
