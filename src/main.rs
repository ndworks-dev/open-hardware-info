#![cfg_attr(not(test), windows_subsystem = "windows")]

mod autostart;
mod config;
mod metrics;
mod render;
mod taskbar;

use config::{Anchor, Config, Rgba};
use metrics::{Command, Metric, Snapshot};
use render::{Cell, Renderer};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, SystemTime};
use taskbar::Taskbar;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const APP_NAME: &str = "Open Hardware Info";
const WM_APP_SAMPLE: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_MENU: u32 = WM_APP + 3;

const ID_METRIC: u32 = 1000;
const ID_MONITOR: u32 = 2000;
const ID_ANCHOR_LEFT: u32 = 3000;
const ID_ANCHOR_RIGHT: u32 = 3001;
const ID_AUTOSTART: u32 = 3100;
const ID_OPEN_CONFIG: u32 = 3200;
const ID_EXIT: u32 = 3900;

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Runs `f` on the app state unless it is already in use. Win32 can re-enter a
/// window procedure from inside calls such as SetWindowPos. Skipping is safe
/// because every sample re-evaluates the whole widget state anyway.
fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|app| {
        if let Ok(mut guard) = app.try_borrow_mut() {
            if let Some(app) = guard.as_mut() {
                f(app);
            }
        }
    });
}

struct App {
    main: HWND,
    widget: HWND,
    parent: HWND,
    renderer: Renderer,
    config: Config,
    config_path: PathBuf,
    config_stamp: Option<SystemTime>,
    config_error: Option<String>,
    snapshot: Snapshot,
    samples: Receiver<Snapshot>,
    commands: Sender<Command>,
    placed: (i32, i32, i32, i32),
    menu_taskbars: Vec<Taskbar>,
    icon: HICON,
}

fn file_stamp(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Returns the config and an error message if the file exists but is invalid.
fn load_config(path: &Path) -> (Config, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => match Config::parse(&text) {
            Ok(config) => (config, None),
            Err(error) => (Config::default(), Some(error)),
        },
        Err(_) => (Config::default(), None),
    }
}

fn system_uses_light_theme() -> bool {
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("SystemUsesLightTheme");
    let mut data = 0u32;
    let mut size = 4u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut data as *mut u32 as *mut _,
            &mut size,
        )
    };
    status == 0 && data != 0
}

impl App {
    fn text_color(&self) -> Rgba {
        Rgba::parse(&self.config.text_color).unwrap_or(if system_uses_light_theme() {
            Rgba { r: 0, g: 0, b: 0, a: 255 }
        } else {
            Rgba { r: 255, g: 255, b: 255, a: 255 }
        })
    }

    fn reload_if_changed(&mut self) {
        let stamp = file_stamp(&self.config_path);
        if stamp == self.config_stamp {
            return;
        }
        self.config_stamp = stamp;
        let (config, error) = load_config(&self.config_path);
        // A file being edited may be briefly invalid: keep what is on screen.
        if error.is_none() {
            self.apply_config(config);
        }
        self.config_error = error;
        self.update_tray(NIM_MODIFY);
    }

    fn apply_config(&mut self, config: Config) {
        if config.interval_ms != self.config.interval_ms {
            let _ = self.commands.send(Command::SetInterval(Duration::from_millis(config.interval_ms)));
        }
        if config.adapter != self.config.adapter {
            let _ = self.commands.send(Command::SetAdapter(config.adapter.clone()));
        }
        self.config = config;
    }

    /// Persists a change made from the menu. A file with errors is left alone:
    /// overwriting it would destroy what the user is still fixing.
    fn save_config(&mut self) {
        if self.config_error.is_none() && self.config.save(&self.config_path).is_ok() {
            self.config_stamp = file_stamp(&self.config_path);
        }
    }

    fn destroy_widget(&mut self) {
        if !self.widget.is_null() {
            unsafe {
                DestroyWindow(self.widget);
            }
        }
        self.widget = null_mut();
        self.parent = null_mut();
        self.placed = (0, 0, 0, 0);
    }

    fn ensure_widget(&mut self, parent: HWND) -> bool {
        let alive = !self.widget.is_null() && unsafe { IsWindow(self.widget) } != 0;
        if alive && self.parent == parent && unsafe { GetParent(self.widget) } == parent {
            return true;
        }
        self.destroy_widget();
        let class = wide("OpenHardwareInfoWidget");
        self.widget = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE,
                class.as_ptr(),
                null(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                parent,
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        self.parent = parent;
        !self.widget.is_null()
    }

    /// Brings the widget in line with the taskbar, config and latest sample.
    /// Also what recovers from Explorer restarts and monitor changes.
    fn refresh(&mut self) {
        self.reload_if_changed();
        let taskbars = taskbar::enumerate();
        let Some(target) = taskbar::select(&taskbars, &self.config.monitor).cloned() else {
            self.destroy_widget();
            return;
        };
        let default_color = self.text_color();
        let cells: Vec<Cell> = self
            .config
            .items
            .iter()
            .filter(|item| item.enabled)
            .map(|item| Cell {
                label: item.label.clone().unwrap_or_else(|| item.metric.default_label().to_owned()),
                value: item.metric.format(self.snapshot.value(item.metric)),
                template: item.metric.template(),
                align_left: matches!(item.metric, Metric::NetDown | Metric::NetUp),
                color: item
                    .color
                    .as_deref()
                    .and_then(|c| Rgba::parse(c).ok())
                    .unwrap_or(default_color),
            })
            .collect();
        if cells.is_empty() {
            self.destroy_widget();
            return;
        }
        if !self.ensure_widget(target.hwnd) {
            return;
        }
        let dpi = unsafe { GetDpiForWindow(target.hwnd) }.max(96);
        let scale = |dips: i32| dips * dpi as i32 / 96;
        let background = Rgba::parse(&self.config.background).unwrap_or(Rgba::TRANSPARENT);
        let (width, height) = self.renderer.render(
            &cells,
            &self.config.font,
            self.config.font_size,
            dpi,
            self.config.rows,
            target.height(),
            background,
        );
        let tray_left = target.tray_left.map(|left| left - target.rect.left);
        let x = taskbar::place_x(
            self.config.anchor,
            target.width(),
            tray_left,
            width,
            scale(self.config.offset),
            scale(self.config.secondary_reserve),
        );
        // Convert through the window itself: the taskbar's client origin need
        // not match its window origin.
        let mut origin = POINT { x: target.rect.left + x, y: target.rect.top };
        unsafe {
            ScreenToClient(target.hwnd, &mut origin);
        }
        let placed = (origin.x, origin.y, width, height);
        let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
        if placed == self.placed {
            flags |= SWP_NOMOVE | SWP_NOSIZE;
        }
        unsafe {
            // Reasserted every time: the taskbar's XAML content can cover us.
            SetWindowPos(self.widget, HWND_TOP, origin.x, origin.y, width, height, flags);
        }
        self.placed = placed;
        self.renderer.present(self.widget);
    }

    fn update_tray(&self, action: u32) {
        let mut data = self.tray_data();
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_APP_TRAY;
        data.hIcon = self.icon;
        let tip = match &self.config_error {
            Some(_) => format!("{APP_NAME}: config.toml inválido"),
            None => APP_NAME.to_owned(),
        };
        for (slot, unit) in data.szTip.iter_mut().take(127).zip(tip.encode_utf16()) {
            *slot = unit;
        }
        unsafe {
            Shell_NotifyIconW(action, &data);
        }
    }

    fn tray_data(&self) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.main;
        data.uID = 1;
        data
    }

    fn build_menu(&mut self) -> HMENU {
        unsafe fn add(menu: HMENU, flags: u32, id: usize, text: &str) {
            AppendMenuW(menu, flags, id, wide(text).as_ptr());
        }
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        unsafe {
            let root = CreatePopupMenu();

            let items = CreatePopupMenu();
            for (i, item) in self.config.items.iter().enumerate() {
                let id = (ID_METRIC + i as u32) as usize;
                add(items, MF_STRING | check(item.enabled), id, item.metric.menu_label());
            }
            add(root, MF_POPUP, items as usize, "Itens exibidos");

            let monitors = CreatePopupMenu();
            self.menu_taskbars = taskbar::enumerate();
            let current = taskbar::select(&self.menu_taskbars, &self.config.monitor).map(|t| t.hwnd);
            for (i, bar) in self.menu_taskbars.iter().enumerate() {
                let id = (ID_MONITOR + i as u32) as usize;
                add(monitors, MF_STRING | check(current == Some(bar.hwnd)), id, &bar.menu_label());
            }
            add(root, MF_POPUP, monitors as usize, "Monitor");

            let side = CreatePopupMenu();
            let anchor = self.config.anchor;
            add(side, MF_STRING | check(anchor == Anchor::Left), ID_ANCHOR_LEFT as usize, "Esquerda");
            add(side, MF_STRING | check(anchor == Anchor::Right), ID_ANCHOR_RIGHT as usize, "Direita");
            add(root, MF_POPUP, side as usize, "Posição");

            AppendMenuW(root, MF_SEPARATOR, 0, null());
            add(root, MF_STRING | check(autostart::is_enabled()), ID_AUTOSTART as usize, "Iniciar com o Windows");
            add(root, MF_STRING, ID_OPEN_CONFIG as usize, "Editar configurações...");
            AppendMenuW(root, MF_SEPARATOR, 0, null());
            add(root, MF_STRING, ID_EXIT as usize, "Sair");
            root
        }
    }

    fn handle_menu_command(&mut self, id: u32) {
        match id {
            ID_EXIT => {
                unsafe {
                    PostMessageW(self.main, WM_CLOSE, 0, 0);
                }
                return;
            }
            ID_AUTOSTART => {
                autostart::set_enabled(!autostart::is_enabled());
            }
            ID_OPEN_CONFIG => self.open_config(),
            ID_ANCHOR_LEFT | ID_ANCHOR_RIGHT => {
                self.config.anchor = if id == ID_ANCHOR_LEFT { Anchor::Left } else { Anchor::Right };
                self.save_config();
            }
            id if (ID_METRIC..ID_MONITOR).contains(&id) => {
                if let Some(item) = self.config.items.get_mut((id - ID_METRIC) as usize) {
                    item.enabled = !item.enabled;
                    self.save_config();
                }
            }
            id if (ID_MONITOR..ID_ANCHOR_LEFT).contains(&id) => {
                if let Some(bar) = self.menu_taskbars.get((id - ID_MONITOR) as usize) {
                    self.config.monitor = if bar.primary { "primary".into() } else { bar.short_device().to_owned() };
                    self.save_config();
                }
            }
            _ => {}
        }
        self.refresh();
    }

    fn open_config(&mut self) {
        if !self.config_path.exists() {
            self.save_config();
        }
        let path = wide(&format!("\"{}\"", self.config_path.display()));
        // notepad, not the "open" verb: .toml often has no associated program.
        let notepad = wide("notepad.exe");
        unsafe {
            ShellExecuteW(null_mut(), null(), notepad.as_ptr(), path.as_ptr(), null(), SW_SHOWNORMAL);
        }
    }
}

/// Tray icon drawn in code, so the build needs no resource compiler.
fn create_icon() -> HICON {
    const SIZE: usize = 32;
    let mut color = vec![0u8; SIZE * SIZE * 4];
    for (column, top) in [(5usize, 20usize), (13, 12), (21, 4)] {
        for y in top..28 {
            for x in column..column + 6 {
                let i = (y * SIZE + x) * 4;
                color[i..i + 4].copy_from_slice(&[0x60, 0xC8, 0x2E, 0xFF]); // BGRA
            }
        }
    }
    let mask = vec![0u8; SIZE * SIZE / 8];
    unsafe {
        CreateIcon(
            GetModuleHandleW(null()),
            SIZE as i32,
            SIZE as i32,
            1,
            32,
            mask.as_ptr(),
            color.as_ptr(),
        )
    }
}

fn show_menu(main: HWND) {
    // No app borrow is held while the menu runs: it pumps messages, and samples
    // must keep flowing behind it.
    let mut menu = null_mut();
    with_app(|app| menu = app.build_menu());
    if menu.is_null() {
        return;
    }
    let command = unsafe {
        let mut at = POINT { x: 0, y: 0 };
        GetCursorPos(&mut at);
        SetForegroundWindow(main); // otherwise the menu ignores clicks outside it
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            at.x,
            at.y,
            0,
            main,
            null(),
        );
        PostMessageW(main, WM_NULL, 0, 0);
        DestroyMenu(menu);
        command
    };
    if command != 0 {
        with_app(|app| app.handle_menu_command(command as u32));
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_SAMPLE => {
            with_app(|app| {
                let mut latest = None;
                while let Ok(snapshot) = app.samples.try_recv() {
                    latest = Some(snapshot);
                }
                if let Some(snapshot) = latest {
                    app.snapshot = snapshot;
                    app.refresh();
                }
            });
            0
        }
        WM_APP_TRAY => {
            if lparam as u32 == WM_RBUTTONUP {
                show_menu(hwnd);
            }
            0
        }
        WM_APP_MENU => {
            show_menu(hwnd);
            0
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            with_app(App::refresh);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ if msg == TASKBAR_CREATED.get() => {
            // Explorer restarted: tray icons are gone and the old parent is dead.
            with_app(|app| {
                app.update_tray(NIM_ADD);
                app.refresh();
            });
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe extern "system" fn widget_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_RBUTTONUP => {
            // Posted: this window shares an input queue with Explorer, so its
            // handler must return immediately rather than run a modal menu.
            with_app(|app| {
                PostMessageW(app.main, WM_APP_MENU, 0, 0);
            });
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn register_class(name: &str, proc_: WNDPROC) {
    let class = wide(name);
    unsafe {
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = proc_;
        wc.hInstance = GetModuleHandleW(null());
        wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
    }
}

fn print_sample() {
    let mut sampler = metrics::Sampler::new(String::new());
    sampler.sample();
    std::thread::sleep(Duration::from_secs(1));
    let snapshot = sampler.sample();
    for &metric in Metric::ALL {
        println!("{:<24}{}", metric.menu_label(), metric.format(snapshot.value(metric)));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--print-sample") {
        print_sample();
        return;
    }
    let config_path = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(config::default_path);

    unsafe {
        // Held until the process exits; a second copy just leaves.
        let mutex = wide("Local\\OpenHardwareInfo.SingleInstance");
        CreateMutexW(null(), 0, mutex.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        TASKBAR_CREATED.set(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()));
    }

    register_class("OpenHardwareInfoMain", Some(main_proc));
    register_class("OpenHardwareInfoWidget", Some(widget_proc));

    // Top-level rather than message-only: message-only windows do not receive
    // the TaskbarCreated broadcast. Never shown.
    let main = unsafe {
        let class = wide("OpenHardwareInfoMain");
        let title = wide(APP_NAME);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        )
    };
    if main.is_null() {
        return;
    }

    let (config, config_error) = load_config(&config_path);
    let main_handle = main as isize;
    let (commands, samples) = metrics::spawn(
        Duration::from_millis(config.interval_ms),
        config.adapter.clone(),
        move || unsafe {
            PostMessageW(main_handle as HWND, WM_APP_SAMPLE, 0, 0);
        },
    );
    let app = App {
        main,
        widget: null_mut(),
        parent: null_mut(),
        renderer: Renderer::new(),
        config,
        config_stamp: file_stamp(&config_path),
        config_path,
        config_error,
        snapshot: Snapshot::default(),
        samples,
        commands,
        placed: (0, 0, 0, 0),
        menu_taskbars: Vec::new(),
        icon: create_icon(),
    };
    app.update_tray(NIM_ADD);
    APP.with(|slot| *slot.borrow_mut() = Some(app));
    with_app(App::refresh);

    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    with_app(|app| {
        let data = app.tray_data();
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &data);
        }
        app.destroy_widget();
    });
}
