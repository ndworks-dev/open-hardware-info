//! The settings window, run as `--settings` in its own process. It only reads
//! and writes the config file; the widget process picks up every save.

use crate::config::{Anchor, Config, FontWeight, Rgba, SYSTEM_FONT};
use crate::{autostart, render, taskbar, wide, MAIN_CLASS, WM_APP_RELOAD};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::rc::Rc;
use std::time::{Duration, SystemTime};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, LPARAM};
use windows_sys::Win32::Graphics::Gdi::{EnumFontFamiliesExW, GetDC, ReleaseDC, DEFAULT_CHARSET, LOGFONTW, TEXTMETRICW};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Controls::Dialogs::{ChooseColorW, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, PostMessageW, SetForegroundWindow, ShowWindow, IDYES, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_YESNO,
    SW_RESTORE, SW_SHOWNORMAL,
};

slint::include_modules!();

pub const WINDOW_TITLE: &str = "Open Hardware Info - Configurações";
const REPOSITORY: &str = "ndworks-dev/open-hardware-info";

/// Options behind the combo boxes: what each visible entry stores in the file.
struct Choices {
    fonts: Vec<String>,
    monitors: Vec<String>,
}

struct State {
    path: PathBuf,
    config: Config,
    stamp: Option<SystemTime>,
    /// The file on disk is invalid. Nothing is saved until the user fixes it or
    /// restores the defaults, so their edits are not overwritten.
    blocked: bool,
    choices: Choices,
}

fn stamp_of(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

unsafe extern "system" fn collect_font(font: *const LOGFONTW, _metrics: *const TEXTMETRICW, _kind: u32, list: LPARAM) -> i32 {
    let face = &(*font).lfFaceName;
    let end = face.iter().position(|&c| c == 0).unwrap_or(face.len());
    (*(list as *mut Vec<String>)).push(String::from_utf16_lossy(&face[..end]));
    1
}

fn installed_fonts() -> Vec<String> {
    let mut fonts = Vec::new();
    unsafe {
        let dc = GetDC(null_mut());
        let mut query: LOGFONTW = std::mem::zeroed();
        query.lfCharSet = DEFAULT_CHARSET as _;
        EnumFontFamiliesExW(dc, &query, Some(collect_font), &mut fonts as *mut Vec<String> as LPARAM, 0);
        ReleaseDC(null_mut(), dc);
    }
    clean_font_list(fonts)
}

/// Sorted and unique, without "@" entries, which are rotated variants for
/// vertical text.
pub fn clean_font_list(mut fonts: Vec<String>) -> Vec<String> {
    fonts.retain(|f| !f.is_empty() && !f.starts_with('@'));
    fonts.sort_by_key(|f| f.to_lowercase());
    fonts.dedup();
    fonts
}

/// Value and visible text for each font, with the Windows default first. A font
/// named in the file but not installed stays listed so it is not silently lost.
pub fn font_choices(installed: &[String], current: &str, system_name: &str) -> (Vec<String>, Vec<String>) {
    let mut values = vec![SYSTEM_FONT.to_owned()];
    let mut names = vec![format!("Padrão do Windows ({system_name})")];
    if current != SYSTEM_FONT && !installed.iter().any(|f| f == current) {
        values.push(current.to_owned());
        names.push(format!("{current} (não instalada)"));
    }
    values.extend(installed.iter().cloned());
    names.extend(installed.iter().cloned());
    (values, names)
}

pub fn layout_index(rows: usize) -> i32 {
    rows.clamp(1, 3) as i32 - 1
}

fn string_model(values: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(values.into_iter().map(SharedString::from).collect::<Vec<_>>()))
}

fn item_rows(config: &Config) -> Vec<ItemRow> {
    config
        .items
        .iter()
        .map(|item| ItemRow {
            name: item.metric.menu_label().into(),
            hint: item.metric.requirement().unwrap_or_default().into(),
            enabled: item.enabled,
            label: item.label.clone().unwrap_or_default().into(),
            default_label: item.metric.default_label().into(),
            color: item.color.clone().unwrap_or_default().into(),
        })
        .collect()
}

fn populate(ui: &SettingsWindow, state: &mut State) {
    let config = &state.config;

    let (values, names) = font_choices(&installed_fonts(), &config.font, &render::system_font_name());
    ui.set_font_names(string_model(names));
    ui.set_font_index(values.iter().position(|v| *v == config.font).unwrap_or(0) as i32);
    state.choices.fonts = values;

    ui.set_font_size(config.font_size.round() as i32);
    ui.set_weight_index(match config.font_weight {
        FontWeight::Regular => 0,
        FontWeight::Semibold => 1,
        FontWeight::Bold => 2,
    });
    ui.set_layout_index(layout_index(config.rows));
    ui.set_text_color(if config.text_color == "auto" { "".into() } else { config.text_color.as_str().into() });
    ui.set_background_color(if config.background == "transparent" { "".into() } else { config.background.as_str().into() });
    ui.set_items(ModelRc::new(VecModel::from(item_rows(config))));

    let taskbars = taskbar::enumerate();
    let mut monitor_values = vec!["primary".to_owned()];
    let mut monitor_names = vec!["Monitor principal".to_owned()];
    for bar in taskbars.iter().filter(|b| !b.primary) {
        monitor_values.push(bar.short_device().to_owned());
        monitor_names.push(bar.short_device().to_owned());
    }
    let known = monitor_values.iter().any(|m| m.eq_ignore_ascii_case(&config.monitor));
    if !known {
        monitor_values.push(config.monitor.clone());
        monitor_names.push(format!("{} (não encontrado)", config.monitor));
    }
    ui.set_monitor_names(string_model(monitor_names));
    ui.set_monitor_index(monitor_values.iter().position(|m| m.eq_ignore_ascii_case(&config.monitor)).unwrap_or(0) as i32);
    state.choices.monitors = monitor_values;

    ui.set_anchor_index(if config.anchor == Anchor::Left { 0 } else { 1 });
    ui.set_offset(config.offset);
    ui.set_secondary_reserve(config.secondary_reserve);
    ui.set_interval(config.interval_ms as i32);
    ui.set_adapter(config.adapter.as_str().into());
    ui.set_autostart(autostart::is_enabled());
}

fn optional(text: SharedString) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The config described by the form. Starts from `base` so that values the form
/// cannot express exactly, like a 9.5pt font, survive unless actually changed.
fn read_form(ui: &SettingsWindow, base: &Config, choices: &Choices) -> Config {
    let mut config = base.clone();
    if let Some(font) = choices.fonts.get(ui.get_font_index() as usize) {
        config.font = font.clone();
    }
    if ui.get_font_size() != base.font_size.round() as i32 {
        config.font_size = ui.get_font_size() as f32;
    }
    config.font_weight = match ui.get_weight_index() {
        1 => FontWeight::Semibold,
        2 => FontWeight::Bold,
        _ => FontWeight::Regular,
    };
    config.rows = ui.get_layout_index() as usize + 1;
    config.text_color = optional(ui.get_text_color()).unwrap_or_else(|| "auto".to_owned());
    config.background = optional(ui.get_background_color()).unwrap_or_else(|| "transparent".to_owned());
    if let Some(monitor) = choices.monitors.get(ui.get_monitor_index() as usize) {
        config.monitor = monitor.clone();
    }
    config.anchor = if ui.get_anchor_index() == 0 { Anchor::Left } else { Anchor::Right };
    config.offset = ui.get_offset();
    config.secondary_reserve = ui.get_secondary_reserve();
    config.interval_ms = ui.get_interval() as u64;
    config.adapter = ui.get_adapter().trim().to_owned();
    let rows = ui.get_items();
    for (item, row) in config.items.iter_mut().zip(rows.iter()) {
        item.enabled = row.enabled;
        item.label = optional(row.label);
        item.color = optional(row.color);
    }
    config
}

fn notify_widget() {
    let class = wide(MAIN_CLASS);
    unsafe {
        let main = FindWindowW(class.as_ptr(), null());
        if !main.is_null() {
            PostMessageW(main, WM_APP_RELOAD, 0, 0);
        }
    }
}

fn show_message(ui: &SettingsWindow, text: &str, is_error: bool) {
    ui.set_message(text.into());
    ui.set_message_is_error(is_error);
}

fn save(ui: &SettingsWindow, state: &mut State) {
    if state.blocked {
        return;
    }
    let mut config = read_form(ui, &state.config, &state.choices);
    // Invalid while typing a color, for instance: keep the last good file and
    // say why, then save as soon as the value becomes valid.
    if let Err(error) = config.normalize() {
        show_message(ui, &error, true);
        return;
    }
    if config == state.config {
        show_message(ui, "", false);
        return;
    }
    match config.save(&state.path) {
        Ok(()) => {
            state.config = config;
            state.stamp = stamp_of(&state.path);
            show_message(ui, "", false);
            notify_widget();
        }
        Err(error) => show_message(ui, &format!("Não foi possível salvar: {error}"), true),
    }
}

/// Reads the file into the state. Returns the error if it is invalid.
fn load(state: &mut State) -> Option<String> {
    state.stamp = stamp_of(&state.path);
    match std::fs::read_to_string(&state.path) {
        Ok(text) => match Config::parse(&text) {
            Ok(config) => {
                state.config = config;
                state.blocked = false;
                None
            }
            Err(error) => {
                state.blocked = true;
                Some(error)
            }
        },
        Err(_) => {
            state.config = Config::default();
            state.blocked = false;
            None
        }
    }
}

fn reload(ui: &SettingsWindow, state: &mut State) {
    let error = load(state);
    populate(ui, state);
    match error {
        Some(error) => {
            show_message(ui, &format!("O config.toml tem um erro e nada será salvo até corrigi-lo ou restaurar os padrões.\n{error}"), true)
        }
        None => show_message(ui, "", false),
    }
}

fn pick_color(current: &str) -> String {
    let parsed = Rgba::parse(current).ok();
    let start = parsed.map_or(0x00FF_FFFF, |c| c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16);
    let mut custom = [0x00FF_FFFFu32; 16];
    let title = wide(WINDOW_TITLE);
    unsafe {
        let mut dialog: CHOOSECOLORW = std::mem::zeroed();
        dialog.lStructSize = std::mem::size_of::<CHOOSECOLORW>() as u32;
        dialog.hwndOwner = FindWindowW(null(), title.as_ptr());
        dialog.rgbResult = start;
        dialog.lpCustColors = custom.as_mut_ptr();
        dialog.Flags = CC_RGBINIT | CC_FULLOPEN;
        if ChooseColorW(&mut dialog) == 0 {
            return current.to_owned();
        }
        let (r, g, b) = (dialog.rgbResult & 0xFF, (dialog.rgbResult >> 8) & 0xFF, (dialog.rgbResult >> 16) & 0xFF);
        // The dialog has no alpha: keep the transparency the color already had.
        match parsed {
            Some(c) if c.a != 255 => format!("#{r:02X}{g:02X}{b:02X}{:02X}", c.a),
            _ => format!("#{r:02X}{g:02X}{b:02X}"),
        }
    }
}

/// PowerShell for the elevated install: the installer of this exact release,
/// which verifies what it downloads. The window stays open so the user can read
/// the result, such as a request to restart Windows.
fn install_command(version: &str) -> String {
    format!(
        "[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12; \
         try {{ & ([scriptblock]::Create((irm 'https://github.com/{REPOSITORY}/releases/download/v{version}/install.ps1'))) -InstallCpuTemp -Version {version} }} \
         catch {{ Write-Host $_ -ForegroundColor Red }} \
         finally {{ Read-Host 'Press Enter to close' }}"
    )
}

/// Asks for consent, then for elevation. This app stays unprivileged and copies
/// nothing itself: a user-writable executable must never feed a SYSTEM service.
fn install_sensor(ui: &SettingsWindow) {
    const USER_CANCELLED_ELEVATION: isize = 5;
    let text = wide(
        "Para ler a temperatura da CPU, o Windows precisa do driver de kernel PawnIO e de um pequeno serviço do Open Hardware Info. \
         A instalação baixa os dois do GitHub e pede permissão de administrador.\n\n\
         Alguns anti-cheats podem bloquear jogos com o PawnIO instalado.\n\nDeseja instalar?",
    );
    let title = wide(WINDOW_TITLE);
    let owner = unsafe { FindWindowW(null(), title.as_ptr()) };
    if unsafe { MessageBoxW(owner, text.as_ptr(), title.as_ptr(), MB_YESNO | MB_ICONINFORMATION | MB_DEFBUTTON2) } != IDYES {
        return;
    }
    let mut system = [0u16; 260];
    let length = unsafe { GetSystemDirectoryW(system.as_mut_ptr(), system.len() as u32) } as usize;
    let powershell = String::from_utf16_lossy(&system[..length.min(system.len())]) + r"\WindowsPowerShell\v1.0\powershell.exe";
    let arguments = format!("-NoProfile -ExecutionPolicy Bypass -Command \"{}\"", install_command(env!("CARGO_PKG_VERSION")));
    let result = unsafe {
        ShellExecuteW(owner, wide("runas").as_ptr(), wide(&powershell).as_ptr(), wide(&arguments).as_ptr(), null(), SW_SHOWNORMAL) as isize
    };
    if result <= 32 && result != USER_CANCELLED_ELEVATION {
        show_message(ui, "Não foi possível iniciar a instalação.", true);
    }
}

pub fn run(path: PathBuf) {
    let title = wide(WINDOW_TITLE);
    unsafe {
        let mutex = wide("Local\\OpenHardwareInfo.Settings");
        CreateMutexW(null(), 0, mutex.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let open = FindWindowW(null(), title.as_ptr());
            if !open.is_null() {
                ShowWindow(open, SW_RESTORE);
                SetForegroundWindow(open);
            }
            return;
        }
    }

    let Ok(ui) = SettingsWindow::new() else { return };
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    let bridge = ui.global::<Bridge>();
    bridge.on_parse_color(|text| match Rgba::parse(text.trim()) {
        Ok(c) => Color::from_argb_u8(c.a, c.r, c.g, c.b),
        Err(_) => Color::from_argb_u8(0, 0, 0, 0),
    });
    bridge.on_pick_color(|current| pick_color(current.trim()).into());

    let state = Rc::new(RefCell::new(State {
        path,
        config: Config::default(),
        stamp: None,
        blocked: false,
        choices: Choices { fonts: Vec::new(), monitors: Vec::new() },
    }));
    reload(&ui, &mut state.borrow_mut());

    ui.on_changed({
        let (ui, state) = (ui.as_weak(), state.clone());
        move || {
            if let Some(ui) = ui.upgrade() {
                save(&ui, &mut state.borrow_mut());
            }
        }
    });
    ui.on_move_item({
        let (ui, state) = (ui.as_weak(), state.clone());
        move |index, delta| {
            let Some(ui) = ui.upgrade() else { return };
            let mut state = state.borrow_mut();
            // Apply pending form edits first so the reorder does not undo them.
            let mut config = read_form(&ui, &state.config, &state.choices);
            let (from, to) = (index as usize, (index + delta) as usize);
            if state.blocked || from >= config.items.len() || to >= config.items.len() {
                return;
            }
            config.items.swap(from, to);
            // A color still being typed must not reach the file.
            if let Err(error) = config.normalize() {
                show_message(&ui, &error, true);
                return;
            }
            ui.set_items(ModelRc::new(VecModel::from(item_rows(&config))));
            match config.save(&state.path) {
                Ok(()) => {
                    state.config = config;
                    state.stamp = stamp_of(&state.path);
                    notify_widget();
                }
                Err(error) => show_message(&ui, &format!("Não foi possível salvar: {error}"), true),
            }
        }
    });
    ui.set_sensor_installed(crate::cpu_temp::service_installed());
    ui.on_install_sensor({
        let ui = ui.as_weak();
        move || {
            if let Some(ui) = ui.upgrade() {
                install_sensor(&ui);
            }
        }
    });
    ui.on_autostart_toggled({
        let ui = ui.as_weak();
        move |enabled| {
            if !autostart::set_enabled(enabled) {
                if let Some(ui) = ui.upgrade() {
                    ui.set_autostart(autostart::is_enabled());
                    show_message(&ui, "Não foi possível alterar o início com o Windows.", true);
                }
            }
        }
    });
    ui.on_open_file({
        let (ui, state) = (ui.as_weak(), state.clone());
        move || {
            let state = state.borrow();
            if !state.path.exists() && state.config.save(&state.path).is_err() {
                if let Some(ui) = ui.upgrade() {
                    show_message(&ui, "Não foi possível criar o config.toml.", true);
                }
                return;
            }
            let file = wide(&format!("\"{}\"", state.path.display()));
            // notepad, not the "open" verb: .toml often has no associated program.
            let notepad = wide("notepad.exe");
            unsafe {
                ShellExecuteW(null_mut(), null(), notepad.as_ptr(), file.as_ptr(), null(), SW_SHOWNORMAL);
            }
        }
    });
    ui.on_restore_defaults({
        let (ui, state) = (ui.as_weak(), state.clone());
        move || {
            let Some(ui) = ui.upgrade() else { return };
            let mut state = state.borrow_mut();
            let defaults = Config::default();
            match defaults.save(&state.path) {
                Ok(()) => {
                    notify_widget();
                    reload(&ui, &mut state);
                }
                Err(error) => show_message(&ui, &format!("Não foi possível salvar: {error}"), true),
            }
        }
    });

    // Follows changes made elsewhere (the tray menu or a text editor), so a
    // later save here does not undo them.
    let watcher = Timer::default();
    watcher.start(TimerMode::Repeated, Duration::from_secs(1), {
        let (ui, state) = (ui.as_weak(), state.clone());
        move || {
            let Some(ui) = ui.upgrade() else { return };
            ui.set_sensor_installed(crate::cpu_temp::service_installed());
            let Ok(mut state) = state.try_borrow_mut() else { return };
            if stamp_of(&state.path) != state.stamp {
                reload(&ui, &mut state);
            }
        }
    });

    let _ = ui.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_command_targets_this_release_and_survives_quoting() {
        let command = install_command("1.2.3");
        assert!(command.contains("/releases/download/v1.2.3/install.ps1"));
        assert!(command.contains("-InstallCpuTemp -Version 1.2.3"));
        // It travels inside a double-quoted -Command argument.
        assert!(!command.contains('"'));
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn font_list_is_sorted_unique_and_hides_vertical_variants() {
        let fonts = clean_font_list(strings(&["Consolas", "@Malgun Gothic", "arial", "Consolas", ""]));
        assert_eq!(fonts, strings(&["arial", "Consolas"]));
    }

    #[test]
    fn windows_default_font_comes_first() {
        let (values, names) = font_choices(&strings(&["Arial"]), SYSTEM_FONT, "Segoe UI");
        assert_eq!(values, strings(&[SYSTEM_FONT, "Arial"]));
        assert_eq!(names[0], "Padrão do Windows (Segoe UI)");
    }

    #[test]
    fn missing_font_stays_selectable() {
        let (values, names) = font_choices(&strings(&["Arial"]), "Fonte Antiga", "Segoe UI");
        assert_eq!(values[1], "Fonte Antiga");
        assert_eq!(names[1], "Fonte Antiga (não instalada)");
    }

    #[test]
    fn layout_index_matches_the_three_options() {
        assert_eq!(layout_index(1), 0);
        assert_eq!(layout_index(2), 1);
        assert_eq!(layout_index(3), 2);
        assert_eq!(layout_index(9), 2);
    }
}
