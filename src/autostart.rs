//! "Start with Windows", through the per-user Run key. Needs no elevation.

use std::ptr::null_mut;
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "OpenHardwareInfo";

fn command() -> Option<Vec<u16>> {
    let exe = std::env::current_exe().ok()?;
    Some(crate::wide(&format!("\"{}\"", exe.display())))
}

/// True only if the entry points at this executable, so a stale entry left by
/// another copy is not reported as active.
pub fn is_enabled() -> bool {
    let (key, value) = (crate::wide(RUN_KEY), crate::wide(VALUE));
    let Some(expected) = command() else { return false };
    let mut buffer = vec![0u16; 1024];
    let mut bytes = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buffer.as_mut_ptr() as *mut _,
            &mut bytes,
        )
    };
    status == 0 && buffer[..bytes as usize / 2] == expected[..]
}

pub fn set_enabled(enabled: bool) -> bool {
    let (key, value) = (crate::wide(RUN_KEY), crate::wide(VALUE));
    unsafe {
        if enabled {
            let Some(data) = command() else { return false };
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                data.as_ptr() as *const _,
                (data.len() * 2) as u32,
            ) == 0
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) == 0
        }
    }
}
