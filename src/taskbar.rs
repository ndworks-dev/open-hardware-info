//! Finds taskbars and works out where the widget goes on them.

use crate::config::Anchor;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetWindowRect,
};

#[derive(Clone)]
pub struct Taskbar {
    pub hwnd: HWND,
    pub device: String,
    pub primary: bool,
    pub rect: RECT,
    /// Left edge of the notification area in screen coordinates. Only the
    /// primary taskbar has a TrayNotifyWnd.
    pub tray_left: Option<i32>,
}

const DEVICE_PREFIX: &str = r"\\.\";

impl Taskbar {
    /// "DISPLAY1" rather than "\\.\DISPLAY1", whose backslashes are awkward in TOML.
    pub fn short_device(&self) -> &str {
        self.device.trim_start_matches(DEVICE_PREFIX)
    }

    pub fn width(&self) -> i32 {
        self.rect.right - self.rect.left
    }

    pub fn height(&self) -> i32 {
        self.rect.bottom - self.rect.top
    }

    pub fn menu_label(&self) -> String {
        let name = self.short_device();
        if self.primary {
            format!("{name} (principal)")
        } else {
            name.to_owned()
        }
    }
}

fn rect_of(hwnd: HWND) -> Option<RECT> {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    (unsafe { GetWindowRect(hwnd, &mut rect) } != 0).then_some(rect)
}

fn describe(hwnd: HWND) -> Option<Taskbar> {
    unsafe {
        let rect = rect_of(hwnd)?;
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) == 0 {
            return None;
        }
        let end = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        let class = crate::wide("TrayNotifyWnd");
        let tray = FindWindowExW(hwnd, std::ptr::null_mut(), class.as_ptr(), std::ptr::null());
        Some(Taskbar {
            hwnd,
            device: String::from_utf16_lossy(&info.szDevice[..end]),
            primary: info.monitorInfo.dwFlags & 1 != 0,
            rect,
            tray_left: if tray.is_null() { None } else { rect_of(tray).map(|r| r.left) },
        })
    }
}

unsafe extern "system" fn collect_secondary(hwnd: HWND, list: LPARAM) -> BOOL {
    let mut class = [0u16; 32];
    let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) as usize;
    if String::from_utf16_lossy(&class[..len]) == "Shell_SecondaryTrayWnd" {
        (*(list as *mut Vec<HWND>)).push(hwnd);
    }
    1
}

/// All taskbars, primary first. Empty while Explorer is restarting.
pub fn enumerate() -> Vec<Taskbar> {
    let mut handles = Vec::new();
    unsafe {
        let class = crate::wide("Shell_TrayWnd");
        let primary = FindWindowW(class.as_ptr(), std::ptr::null());
        if !primary.is_null() {
            handles.push(primary);
        }
        EnumWindows(Some(collect_secondary), &mut handles as *mut Vec<HWND> as LPARAM);
    }
    handles.into_iter().filter_map(describe).collect()
}

/// `monitor` is "primary" or a device name. An unplugged monitor falls back to
/// the primary taskbar so the widget does not vanish.
pub fn select<'a>(taskbars: &'a [Taskbar], monitor: &str) -> Option<&'a Taskbar> {
    taskbars
        .iter()
        .find(|t| t.short_device().eq_ignore_ascii_case(monitor.trim_start_matches(DEVICE_PREFIX)))
        .or_else(|| taskbars.iter().find(|t| t.primary))
        .or_else(|| taskbars.first())
}

/// Left edge of the widget in the taskbar's coordinates. All lengths are
/// physical pixels. The result is kept inside the taskbar.
pub fn place_x(
    anchor: Anchor,
    taskbar_width: i32,
    tray_left: Option<i32>,
    widget_width: i32,
    offset: i32,
    secondary_reserve: i32,
) -> i32 {
    let x = match anchor {
        Anchor::Left => offset,
        Anchor::Right => tray_left.unwrap_or(taskbar_width - secondary_reserve) - widget_width - offset,
    };
    x.clamp(0, (taskbar_width - widget_width).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(device: &str, primary: bool) -> Taskbar {
        Taskbar {
            hwnd: std::ptr::null_mut(),
            device: device.into(),
            primary,
            rect: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            tray_left: None,
        }
    }

    #[test]
    fn select_finds_monitor_and_falls_back_to_primary() {
        let bars = [bar(r"\\.\DISPLAY2", true), bar(r"\\.\DISPLAY1", false)];
        assert_eq!(select(&bars, r"\\.\display1").unwrap().device, r"\\.\DISPLAY1");
        assert_eq!(select(&bars, "DISPLAY1").unwrap().device, r"\\.\DISPLAY1");
        assert_eq!(select(&bars, "primary").unwrap().device, r"\\.\DISPLAY2");
        assert_eq!(select(&bars, r"\\.\DISPLAY9").unwrap().device, r"\\.\DISPLAY2");
        assert!(select(&[], "primary").is_none());
    }

    #[test]
    fn right_anchor_sits_left_of_the_tray() {
        assert_eq!(place_x(Anchor::Right, 1920, Some(1546), 200, 8, 190), 1338);
    }

    #[test]
    fn right_anchor_without_tray_uses_the_reserve() {
        assert_eq!(place_x(Anchor::Right, 1920, None, 200, 8, 190), 1522);
    }

    #[test]
    fn left_anchor_uses_the_offset() {
        assert_eq!(place_x(Anchor::Left, 1920, Some(1546), 200, 8, 190), 8);
    }

    #[test]
    fn placement_stays_inside_the_taskbar() {
        assert_eq!(place_x(Anchor::Right, 300, Some(100), 200, 8, 190), 0);
        assert_eq!(place_x(Anchor::Left, 100, None, 200, 8, 190), 0);
    }
}
