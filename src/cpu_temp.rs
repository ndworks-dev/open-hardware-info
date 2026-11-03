//! Reads the CPU temperature published by the optional `ohi-sensor` service.
//! The widget itself never touches the driver and needs no elevation.

use crate::sensor_shared::{decode, MAPPING_NAME, MAPPING_SIZE, SERVICE_NAME};
use std::sync::atomic::{AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Memory::{MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS};
use windows_sys::Win32::System::Services::{CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;

/// Whether the service exists, running or not. Needs no elevation.
pub fn service_installed() -> bool {
    unsafe {
        let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if manager.is_null() {
            return false;
        }
        let name = crate::wide(SERVICE_NAME);
        let service = OpenServiceW(manager, name.as_ptr(), SERVICE_QUERY_STATUS);
        let installed = !service.is_null();
        if installed {
            CloseServiceHandle(service);
        }
        CloseServiceHandle(manager);
        installed
    }
}

struct View {
    mapping: HANDLE,
    address: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl View {
    fn open() -> Option<View> {
        unsafe {
            let name = crate::wide(MAPPING_NAME);
            let mapping = OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr());
            if mapping.is_null() {
                return None;
            }
            let address = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, MAPPING_SIZE as usize);
            if address.Value.is_null() {
                CloseHandle(mapping);
                return None;
            }
            Some(View { mapping, address })
        }
    }

    fn word(&self) -> u64 {
        // A mapped view is page-aligned, and the service only ever stores this
        // word atomically.
        unsafe { (*(self.address.Value as *const AtomicU64)).load(Ordering::Acquire) }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.address);
            CloseHandle(self.mapping);
        }
    }
}

/// The service may be installed or started while the widget is running, so a
/// missing mapping is retried on every read.
#[derive(Default)]
pub struct CpuTempReader {
    view: Option<View>,
}

impl CpuTempReader {
    pub fn read(&mut self) -> Option<f64> {
        if self.view.is_none() {
            self.view = View::open();
        }
        let word = self.view.as_ref()?.word();
        decode(word, unsafe { GetTickCount64() })
    }
}
