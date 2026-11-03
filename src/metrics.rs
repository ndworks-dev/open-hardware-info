//! Sensor sampling. Everything here runs on a worker thread and uses only
//! user-mode APIs: no driver, no administrator rights, no bundled DLL.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT_COUNTERVALUE,
    PDH_FMT_DOUBLE,
};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    NetDown,
    NetUp,
    Cpu,
    Ram,
    Gpu,
    GpuTemp,
    Vram,
    Disk,
}

impl Metric {
    pub const ALL: &'static [Metric] =
        &[Metric::NetDown, Metric::NetUp, Metric::Cpu, Metric::Ram, Metric::Gpu, Metric::GpuTemp, Metric::Vram, Metric::Disk];

    pub fn default_enabled(self) -> bool {
        !matches!(self, Metric::Vram | Metric::Disk)
    }

    pub fn default_label(self) -> &'static str {
        match self {
            Metric::NetDown => "↓",
            Metric::NetUp => "↑",
            Metric::Cpu => "CPU",
            Metric::Ram => "RAM",
            Metric::Gpu => "GPU",
            Metric::GpuTemp => "GPU",
            Metric::Vram => "VRAM",
            Metric::Disk => "Disco",
        }
    }

    pub fn menu_label(self) -> &'static str {
        match self {
            Metric::NetDown => "Download",
            Metric::NetUp => "Upload",
            Metric::Cpu => "Uso da CPU",
            Metric::Ram => "Uso da memória",
            Metric::Gpu => "Uso da GPU",
            Metric::GpuTemp => "Temperatura da GPU",
            Metric::Vram => "Uso da memória da GPU",
            Metric::Disk => "Atividade do disco",
        }
    }

    /// Widest expected value: reserves column width so text does not jitter.
    pub fn template(self) -> &'static str {
        match self {
            Metric::NetDown | Metric::NetUp => "99.9 MB/s",
            Metric::GpuTemp => "100°C",
            _ => "100%",
        }
    }

    pub fn format(self, value: Option<f64>) -> String {
        let Some(value) = value else { return "--".into() };
        match self {
            Metric::NetDown | Metric::NetUp => format_rate(value),
            Metric::GpuTemp => format!("{value:.0}°C"),
            _ => format!("{value:.0}%"),
        }
    }
}

pub fn format_rate(bytes_per_second: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes_per_second.max(0.0);
    // Compare against what rounding will display, so 1023.99 KB/s reads
    // "1.0 MB/s" rather than "1024 KB/s".
    if b < 999.5 * KB {
        format!("{:.0} KB/s", b / KB)
    } else if b < 999.95 * MB {
        format!("{:.1} MB/s", b / MB)
    } else {
        format!("{:.1} GB/s", b / GB)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub net_down: Option<f64>,
    pub net_up: Option<f64>,
    pub cpu: Option<f64>,
    pub ram: Option<f64>,
    pub gpu: Option<f64>,
    pub gpu_temp: Option<f64>,
    pub vram: Option<f64>,
    pub disk: Option<f64>,
}

impl Snapshot {
    pub fn value(&self, metric: Metric) -> Option<f64> {
        match metric {
            Metric::NetDown => self.net_down,
            Metric::NetUp => self.net_up,
            Metric::Cpu => self.cpu,
            Metric::Ram => self.ram,
            Metric::Gpu => self.gpu,
            Metric::GpuTemp => self.gpu_temp,
            Metric::Vram => self.vram,
            Metric::Disk => self.disk,
        }
    }
}

// ---------------------------------------------------------------- network

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IfCounters {
    pub luid: u64,
    pub rx: u64,
    pub tx: u64,
}

/// Bytes moved since `previous`. Interfaces that just appeared contribute
/// nothing, and a counter that went backwards (adapter reset) counts as zero,
/// so neither shows up as a huge spike.
pub fn counter_delta(previous: &HashMap<u64, (u64, u64)>, current: &[IfCounters]) -> (u64, u64) {
    current.iter().fold((0, 0), |(rx, tx), c| match previous.get(&c.luid) {
        Some(&(prev_rx, prev_tx)) => (rx + c.rx.saturating_sub(prev_rx), tx + c.tx.saturating_sub(prev_tx)),
        None => (rx, tx),
    })
}

fn read_interfaces(adapter: &str) -> Option<Vec<IfCounters>> {
    const HARDWARE_INTERFACE: u8 = 0b01;
    const FILTER_INTERFACE: u8 = 0b10;
    const OPER_STATUS_UP: i32 = 1;
    const LOOPBACK: u32 = 24;

    let needle = adapter.to_lowercase();
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table) != 0 || table.is_null() {
            return None;
        }
        let count = (*table).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), count);
        let mut out = Vec::new();
        for row in rows {
            let flags = row.InterfaceAndOperStatusFlags._bitfield;
            // One physical NIC also appears as several filter pseudo-interfaces
            // with the same traffic; keeping only the real one avoids counting
            // it multiple times.
            let physical = flags & HARDWARE_INTERFACE != 0 && flags & FILTER_INTERFACE == 0;
            if !physical || row.OperStatus as i32 != OPER_STATUS_UP || row.Type == LOOPBACK {
                continue;
            }
            if !needle.is_empty() {
                let name = wide_field(&row.Alias) + " " + &wide_field(&row.Description);
                if !name.to_lowercase().contains(&needle) {
                    continue;
                }
            }
            out.push(IfCounters { luid: row.InterfaceLuid.Value, rx: row.InOctets, tx: row.OutOctets });
        }
        FreeMibTable(table as *const _);
        Some(out)
    }
}

fn wide_field(field: &[u16]) -> String {
    let end = field.iter().position(|&c| c == 0).unwrap_or(field.len());
    String::from_utf16_lossy(&field[..end])
}

// -------------------------------------------------------------------- PDH

/// Performance counters. Paths are added through the *English* API because
/// counter names are localized on pt-BR Windows.
struct Pdh {
    query: isize,
    cpu: Option<isize>,
    disk_idle: Option<isize>,
}

impl Pdh {
    fn open() -> Option<Pdh> {
        unsafe {
            let mut query = std::mem::zeroed();
            if PdhOpenQueryW(std::ptr::null(), 0, &mut query) != 0 {
                return None;
            }
            let add = |path: &str| -> Option<isize> {
                let wide = crate::wide(path);
                let mut counter = std::mem::zeroed();
                (PdhAddEnglishCounterW(query, wide.as_ptr(), 0, &mut counter) == 0).then_some(counter as isize)
            };
            let cpu = add(r"\Processor Information(_Total)\% Processor Utility");
            let disk_idle = add(r"\PhysicalDisk(_Total)\% Idle Time");
            PdhCollectQueryData(query);
            Some(Pdh { query: query as isize, cpu, disk_idle })
        }
    }

    fn collect(&self) -> (Option<f64>, Option<f64>) {
        unsafe {
            PdhCollectQueryData(self.query as _);
        }
        let cpu = self.cpu.and_then(read_counter).map(|v| v.clamp(0.0, 100.0));
        let disk = self.disk_idle.and_then(read_counter).map(|idle| (100.0 - idle).clamp(0.0, 100.0));
        (cpu, disk)
    }
}

impl Drop for Pdh {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query as _);
        }
    }
}

fn read_counter(counter: isize) -> Option<f64> {
    unsafe {
        let mut value: PDH_FMT_COUNTERVALUE = std::mem::zeroed();
        let status = PdhGetFormattedCounterValue(counter as _, PDH_FMT_DOUBLE, std::ptr::null_mut(), &mut value);
        (status == 0 && value.CStatus == 0).then(|| value.Anonymous.doubleValue)
    }
}

// ------------------------------------------------------------------- NVML

#[repr(C)]
#[derive(Default)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

#[repr(C)]
#[derive(Default)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

type Device = *mut std::ffi::c_void;

/// NVIDIA's management library, installed with the display driver. Loaded only
/// from System32 and absent on non-NVIDIA machines, where GPU items show "--".
struct Nvml {
    module: HMODULE,
    device: Device,
    utilization: unsafe extern "system" fn(Device, *mut NvmlUtilization) -> u32,
    temperature: unsafe extern "system" fn(Device, u32, *mut u32) -> u32,
    memory: unsafe extern "system" fn(Device, *mut NvmlMemory) -> u32,
    shutdown: unsafe extern "system" fn() -> u32,
}

impl Nvml {
    fn load() -> Option<Nvml> {
        unsafe {
            let name = crate::wide("nvml.dll");
            let module = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
            if module.is_null() {
                return None;
            }
            let nvml = Self::bind(module);
            if nvml.is_none() {
                FreeLibrary(module);
            }
            nvml
        }
    }

    unsafe fn bind(module: HMODULE) -> Option<Nvml> {
        macro_rules! symbol {
            ($name:literal) => {
                std::mem::transmute(GetProcAddress(module, concat!($name, "\0").as_ptr())?)
            };
        }
        let init: unsafe extern "system" fn() -> u32 = symbol!("nvmlInit_v2");
        let by_index: unsafe extern "system" fn(u32, *mut Device) -> u32 = symbol!("nvmlDeviceGetHandleByIndex_v2");
        let utilization = symbol!("nvmlDeviceGetUtilizationRates");
        let temperature = symbol!("nvmlDeviceGetTemperature");
        let memory = symbol!("nvmlDeviceGetMemoryInfo");
        let shutdown: unsafe extern "system" fn() -> u32 = symbol!("nvmlShutdown");
        if init() != 0 {
            return None;
        }
        let mut device = std::ptr::null_mut();
        if by_index(0, &mut device) != 0 {
            shutdown();
            return None;
        }
        Some(Nvml { module, device, utilization, temperature, memory, shutdown })
    }

    fn usage(&self) -> Option<f64> {
        let mut u = NvmlUtilization::default();
        unsafe { ((self.utilization)(self.device, &mut u) == 0).then_some(u.gpu as f64) }
    }

    fn temperature(&self) -> Option<f64> {
        const SENSOR_GPU: u32 = 0;
        let mut t = 0;
        unsafe { ((self.temperature)(self.device, SENSOR_GPU, &mut t) == 0).then_some(t as f64) }
    }

    fn memory_percent(&self) -> Option<f64> {
        let mut m = NvmlMemory::default();
        let ok = unsafe { (self.memory)(self.device, &mut m) == 0 };
        (ok && m.total > 0).then(|| m.used as f64 * 100.0 / m.total as f64)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
            FreeLibrary(self.module);
        }
    }
}

// ---------------------------------------------------------------- sampler

fn ram_percent() -> Option<f64> {
    unsafe {
        let mut status: MEMORYSTATUSEX = std::mem::zeroed();
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut status) == 0 || status.ullTotalPhys == 0 {
            return None;
        }
        let used = status.ullTotalPhys - status.ullAvailPhys;
        Some(used as f64 * 100.0 / status.ullTotalPhys as f64)
    }
}

pub struct Sampler {
    adapter: String,
    pdh: Option<Pdh>,
    nvml: Option<Nvml>,
    net_previous: HashMap<u64, (u64, u64)>,
    net_time: Option<Instant>,
}

impl Sampler {
    pub fn new(adapter: String) -> Sampler {
        Sampler { adapter, pdh: Pdh::open(), nvml: Nvml::load(), net_previous: HashMap::new(), net_time: None }
    }

    pub fn set_adapter(&mut self, adapter: String) {
        self.adapter = adapter;
        self.net_previous.clear();
        self.net_time = None;
    }

    pub fn sample(&mut self) -> Snapshot {
        let (net_down, net_up) = self.sample_network();
        let (cpu, disk) = self.pdh.as_ref().map_or((None, None), Pdh::collect);
        let nvml = self.nvml.as_ref();
        Snapshot {
            net_down,
            net_up,
            cpu,
            ram: ram_percent(),
            gpu: nvml.and_then(Nvml::usage),
            gpu_temp: nvml.and_then(Nvml::temperature),
            vram: nvml.and_then(Nvml::memory_percent),
            disk,
        }
    }

    fn sample_network(&mut self) -> (Option<f64>, Option<f64>) {
        let Some(current) = read_interfaces(&self.adapter) else { return (None, None) };
        let now = Instant::now();
        let rates = self.net_time.map(|then| {
            let elapsed = now.duration_since(then).as_secs_f64().max(0.001);
            let (rx, tx) = counter_delta(&self.net_previous, &current);
            (rx as f64 / elapsed, tx as f64 / elapsed)
        });
        self.net_previous = current.iter().map(|c| (c.luid, (c.rx, c.tx))).collect();
        self.net_time = Some(now);
        (rates.map(|r| r.0), rates.map(|r| r.1))
    }
}

pub enum Command {
    SetInterval(Duration),
    SetAdapter(String),
}

/// Samples on its own thread: slow sensors must never stall the UI thread, whose
/// window lives inside the taskbar and shares its input queue with Explorer.
/// Dropping the returned sender stops the thread.
pub fn spawn(interval: Duration, adapter: String, notify: impl Fn() + Send + 'static) -> (Sender<Command>, Receiver<Snapshot>) {
    let (command_tx, command_rx) = mpsc::channel();
    let (snapshot_tx, snapshot_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sampler = Sampler::new(adapter);
        let mut interval = interval;
        loop {
            if snapshot_tx.send(sampler.sample()).is_err() {
                return;
            }
            notify();
            match command_rx.recv_timeout(interval) {
                Ok(Command::SetInterval(new)) => interval = new,
                Ok(Command::SetAdapter(new)) => sampler.set_adapter(new),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    (command_tx, snapshot_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_are_formatted_without_impossible_values() {
        assert_eq!(format_rate(0.0), "0 KB/s");
        assert_eq!(format_rate(-5.0), "0 KB/s");
        assert_eq!(format_rate(512.0 * 1024.0), "512 KB/s");
        assert_eq!(format_rate(1023.99 * 1024.0), "1.0 MB/s");
        assert_eq!(format_rate(1.5 * 1024.0 * 1024.0), "1.5 MB/s");
        assert_eq!(format_rate(2048.0 * 1024.0 * 1024.0), "2.0 GB/s");
    }

    #[test]
    fn missing_values_render_as_dashes() {
        assert_eq!(Metric::Gpu.format(None), "--");
        assert_eq!(Metric::Gpu.format(Some(42.4)), "42%");
        assert_eq!(Metric::GpuTemp.format(Some(61.0)), "61°C");
    }

    #[test]
    fn counter_delta_sums_known_interfaces_only() {
        let previous = HashMap::from([(1, (100, 10)), (2, (500, 50))]);
        let current = [
            IfCounters { luid: 1, rx: 150, tx: 15 },
            IfCounters { luid: 2, rx: 600, tx: 70 },
            IfCounters { luid: 3, rx: 9_000_000, tx: 9_000_000 },
        ];
        assert_eq!(counter_delta(&previous, &current), (150, 25));
    }

    #[test]
    fn counter_reset_does_not_produce_a_spike() {
        let previous = HashMap::from([(1, (1_000_000, 1_000_000))]);
        let current = [IfCounters { luid: 1, rx: 10, tx: 10 }];
        assert_eq!(counter_delta(&previous, &current), (0, 0));
    }
}
