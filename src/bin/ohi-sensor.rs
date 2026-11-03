//! Optional Windows service behind the "CPU temperature" metric. PawnIO only
//! accepts SYSTEM and administrators, so this small binary reads the sensor and
//! publishes the value for the unprivileged widget.
//!
//! It runs as SYSTEM, so it must stay installed under %ProgramFiles%, never in a
//! user-writable folder, and it takes no input from other processes.

#[path = "../sensor_shared.rs"]
#[allow(dead_code)] // Each side of the protocol uses only its half.
mod sensor_shared;

use sensor_shared::{encode, is_plausible, MAPPING_NAME, MAPPING_SIZE, SERVICE_NAME};
use std::arch::x86_64::__cpuid;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_NOT_SUPPORTED, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, NO_ERROR,
    WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::Memory::{CreateFileMappingW, MapViewOfFile, FILE_MAP_WRITE, PAGE_READWRITE};
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerW, SetServiceStatus, StartServiceCtrlDispatcherW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE,
    SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW, ReleaseMutex, SetEvent, WaitForSingleObject};
use windows_sys::Win32::System::IO::DeviceIoControl;

const POLL_MS: u32 = 1_000;

// PawnIO modules are loaded only if their signature is valid, so these bytes
// must stay exactly as released. See licenses/LGPL-2.1-PawnIO-Modules.txt.
static AMD_MODULE: &[u8] = include_bytes!("../../assets/pawnio/AMDFamily17.bin");
static INTEL_MODULE: &[u8] = include_bytes!("../../assets/pawnio/IntelMSR.bin");

// ------------------------------------------------------------- CPU decoding

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cpu {
    Amd { tctl_offset: f64 },
    Intel,
}

fn cpu_family(signature: u32) -> u32 {
    let base = (signature >> 8) & 0xF;
    if base == 0xF {
        base + ((signature >> 20) & 0xFF)
    } else {
        base
    }
}

/// Some early parts report Tctl above the real die temperature. Same table as
/// Linux's k10temp driver.
fn amd_tctl_offset(brand: &str) -> f64 {
    if ["1600X", "1700X", "1800X"].iter().any(|model| brand.contains(model)) {
        -20.0
    } else if ["Threadripper 19", "Threadripper 29"].iter().any(|model| brand.contains(model)) {
        -27.0
    } else if brand.contains("2700X") {
        -10.0
    } else {
        0.0
    }
}

fn amd_celsius(raw: u32, tctl_offset: f64) -> f64 {
    const RANGE_SEL: u32 = 0x8_0000;
    const TJ_SEL: u32 = 0x3_0000;
    let adjusted = raw & RANGE_SEL != 0 || raw & TJ_SEL == TJ_SEL;
    f64::from(raw >> 21) * 0.125 + if adjusted { -49.0 } else { 0.0 } + tctl_offset
}

/// The MSRs report how far below TjMax the package is, in whole degrees.
fn intel_celsius(package_status: u64, temperature_target: u64) -> Option<f64> {
    let tj_max = (temperature_target >> 16) & 0xFF;
    let below_tj_max = (package_status >> 16) & 0x7F;
    (tj_max != 0).then_some(tj_max as f64 - below_tj_max as f64)
}

fn detect_cpu() -> Option<Cpu> {
    const AMD_FAMILIES: std::ops::RangeInclusive<u32> = 0x17..=0x1A;
    const PACKAGE_THERMAL_SENSOR: u32 = 1 << 6;

    let leaf0 = __cpuid(0);
    let vendor: Vec<u8> = [leaf0.ebx, leaf0.edx, leaf0.ecx].iter().flat_map(|r| r.to_le_bytes()).collect();
    match vendor.as_slice() {
        b"AuthenticAMD" if AMD_FAMILIES.contains(&cpu_family(__cpuid(1).eax)) => {
            let brand: Vec<u8> = (0x8000_0002..=0x8000_0004u32)
                .flat_map(|leaf| {
                    let r = __cpuid(leaf);
                    [r.eax, r.ebx, r.ecx, r.edx]
                })
                .flat_map(u32::to_le_bytes)
                .collect();
            Some(Cpu::Amd { tctl_offset: amd_tctl_offset(&String::from_utf8_lossy(&brand)) })
        }
        b"GenuineIntel" if leaf0.eax >= 6 && __cpuid(6).eax & PACKAGE_THERMAL_SENSOR != 0 => Some(Cpu::Intel),
        _ => None,
    }
}

// ------------------------------------------------------------------- PawnIO

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct PawnIo(Handle);

impl PawnIo {
    const DEVICE_TYPE: u32 = 41394 << 16;
    const IOCTL_LOAD: u32 = Self::DEVICE_TYPE | (0x821 << 2);
    const IOCTL_EXECUTE: u32 = Self::DEVICE_TYPE | (0x841 << 2);
    const FUNCTION_NAME_SIZE: usize = 32;

    fn open(module: &[u8]) -> Result<PawnIo, u32> {
        let path = wide(r"\\?\GLOBALROOT\Device\PawnIO");
        let device = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if device == INVALID_HANDLE_VALUE {
            return Err(unsafe { GetLastError() });
        }
        let device = PawnIo(Handle(device));
        let mut returned = 0;
        let loaded = unsafe {
            DeviceIoControl(
                device.0 .0,
                Self::IOCTL_LOAD,
                module.as_ptr().cast(),
                module.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if loaded == 0 {
            return Err(unsafe { GetLastError() });
        }
        Ok(device)
    }

    fn execute(&self, function: &str, argument: u64) -> Result<u64, u32> {
        let mut input = [0u8; Self::FUNCTION_NAME_SIZE + 8];
        input[..function.len()].copy_from_slice(function.as_bytes());
        input[Self::FUNCTION_NAME_SIZE..].copy_from_slice(&argument.to_le_bytes());
        let mut output = [0u8; 8];
        let mut returned = 0;
        let ok = unsafe {
            DeviceIoControl(
                self.0 .0,
                Self::IOCTL_EXECUTE,
                input.as_ptr().cast(),
                input.len() as u32,
                output.as_mut_ptr().cast(),
                output.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(unsafe { GetLastError() });
        }
        Ok(u64::from_le_bytes(output))
    }
}

// ------------------------------------------------------------------- sensor

/// Reading the SMN takes two PCI config accesses that other monitoring tools
/// also make, so they all coordinate through this well-known mutex.
struct PciLock(Handle);

impl PciLock {
    fn open() -> Result<PciLock, u32> {
        let name = wide(r"Global\Access_PCI");
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if mutex.is_null() {
            return Err(unsafe { GetLastError() });
        }
        Ok(PciLock(Handle(mutex)))
    }

    fn with<T>(&self, read: impl FnOnce() -> T) -> Option<T> {
        const TIMEOUT_MS: u32 = 100;
        unsafe {
            let wait = WaitForSingleObject(self.0 .0, TIMEOUT_MS);
            if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
                return None;
            }
            let result = read();
            ReleaseMutex(self.0 .0);
            Some(result)
        }
    }
}

enum Sensor {
    Amd { device: PawnIo, lock: PciLock, tctl_offset: f64 },
    Intel { device: PawnIo },
}

impl Sensor {
    fn open(cpu: Cpu) -> Result<Sensor, u32> {
        match cpu {
            Cpu::Amd { tctl_offset } => Ok(Sensor::Amd { device: PawnIo::open(AMD_MODULE)?, lock: PciLock::open()?, tctl_offset }),
            Cpu::Intel => Ok(Sensor::Intel { device: PawnIo::open(INTEL_MODULE)? }),
        }
    }

    /// `Ok(None)` is a skipped reading (bus busy); `Err` means the device failed.
    fn read(&self) -> Result<Option<f64>, u32> {
        const THM_TCON_CUR_TMP: u64 = 0x0005_9800;
        const MSR_TEMPERATURE_TARGET: u64 = 0x1A2;
        const MSR_PACKAGE_THERM_STATUS: u64 = 0x1B1;
        match self {
            Sensor::Amd { device, lock, tctl_offset } => match lock.with(|| device.execute("ioctl_read_smn", THM_TCON_CUR_TMP)) {
                Some(raw) => Ok(Some(amd_celsius(raw? as u32, *tctl_offset))),
                None => Ok(None),
            },
            Sensor::Intel { device } => {
                let target = device.execute("ioctl_read_msr", MSR_TEMPERATURE_TARGET)?;
                let status = device.execute("ioctl_read_msr", MSR_PACKAGE_THERM_STATUS)?;
                Ok(intel_celsius(status, target))
            }
        }
    }
}

// ---------------------------------------------------------------- publisher

struct Publisher {
    _mapping: Handle,
    word: *const AtomicU64,
}

impl Publisher {
    fn create() -> Result<Publisher, u32> {
        // SYSTEM has full access, signed-in users can only read. Creating a
        // Global\ object needs a privilege normal users lack, so they cannot
        // get there first and own the mapping.
        let sddl = wide("D:P(A;;GA;;;SY)(A;;GR;;;AU)");
        unsafe {
            let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut descriptor, std::ptr::null_mut())
                == 0
            {
                return Err(GetLastError());
            }
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let name = wide(MAPPING_NAME);
            let mapping = CreateFileMappingW(INVALID_HANDLE_VALUE, &attributes, PAGE_READWRITE, 0, MAPPING_SIZE, name.as_ptr());
            let error = GetLastError();
            LocalFree(descriptor);
            if mapping.is_null() {
                return Err(error);
            }
            let mapping = Handle(mapping);
            let address = MapViewOfFile(mapping.0, FILE_MAP_WRITE, 0, 0, MAPPING_SIZE as usize);
            if address.Value.is_null() {
                return Err(GetLastError());
            }
            Ok(Publisher { _mapping: mapping, word: address.Value as *const AtomicU64 })
        }
    }

    fn store(&self, word: u64) {
        unsafe { (*self.word).store(word, Ordering::Release) }
    }
}

fn poll(cpu: Cpu, stop: HANDLE) -> Result<(), u32> {
    let publisher = Publisher::create()?;
    let mut sensor = None;
    loop {
        if sensor.is_none() {
            // PawnIO may start after us, or be installed later.
            sensor = Sensor::open(cpu).ok();
        }
        if let Some(active) = &sensor {
            match active.read() {
                Ok(Some(celsius)) if is_plausible(celsius) => publisher.store(encode(celsius, unsafe { GetTickCount64() })),
                Ok(_) => {}
                Err(_) => sensor = None,
            }
        }
        if unsafe { WaitForSingleObject(stop, POLL_MS) } == WAIT_OBJECT_0 {
            publisher.store(0);
            return Ok(());
        }
    }
}

// ------------------------------------------------------------------ service

static STATUS_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static STOP_EVENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

fn report(state: SERVICE_STATUS_CURRENT_STATE, exit_code: u32) {
    let accepted = if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN } else { 0 };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: exit_code,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: if state == SERVICE_RUNNING || state == SERVICE_STOPPED { 0 } else { 3_000 },
    };
    unsafe {
        SetServiceStatus(STATUS_HANDLE.load(Ordering::Acquire), &status);
    }
}

unsafe extern "system" fn control_handler(control: u32) {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        report(SERVICE_STOP_PENDING, NO_ERROR);
        SetEvent(STOP_EVENT.load(Ordering::Acquire));
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let name = wide(SERVICE_NAME);
    let handle = RegisterServiceCtrlHandlerW(name.as_ptr(), Some(control_handler));
    if handle.is_null() {
        return;
    }
    STATUS_HANDLE.store(handle, Ordering::Release);
    report(SERVICE_START_PENDING, NO_ERROR);

    let stop = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
    if stop.is_null() {
        report(SERVICE_STOPPED, GetLastError());
        return;
    }
    STOP_EVENT.store(stop, Ordering::Release);

    let Some(cpu) = detect_cpu() else {
        report(SERVICE_STOPPED, ERROR_NOT_SUPPORTED);
        return;
    };
    report(SERVICE_RUNNING, NO_ERROR);
    let exit_code = poll(cpu, stop).err().unwrap_or(NO_ERROR);
    report(SERVICE_STOPPED, exit_code);
}

/// One reading on stdout, to diagnose a machine from an elevated terminal.
fn print_reading() -> Result<(), String> {
    let cpu = detect_cpu().ok_or("CPU not supported")?;
    println!("cpu: {cpu:?}");
    let sensor = Sensor::open(cpu).map_err(|code| format!("could not open PawnIO: Windows error {code}"))?;
    match sensor.read().map_err(|code| format!("could not read the sensor: Windows error {code}"))? {
        Some(celsius) => println!("temperature: {celsius:.1} C"),
        None => println!("temperature: no reading"),
    }
    Ok(())
}

fn main() {
    if std::env::args().any(|arg| arg == "--print") {
        if let Err(message) = print_reading() {
            eprintln!("{message}");
            std::process::exit(1);
        }
        return;
    }
    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW { lpServiceName: name.as_mut_ptr(), lpServiceProc: Some(service_main) },
        SERVICE_TABLE_ENTRYW { lpServiceName: std::ptr::null_mut(), lpServiceProc: None },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        eprintln!("This is a Windows service. Use --print for a single reading from an elevated terminal.");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_includes_the_extended_bits_only_for_base_15() {
        assert_eq!(cpu_family(0x00A2_0F10), 0x19); // Ryzen 5 5600X
        assert_eq!(cpu_family(0x0080_0F11), 0x17); // Zen
        assert_eq!(cpu_family(0x0009_06EA), 6); // Intel Coffee Lake
    }

    #[test]
    fn amd_values_match_a_reading_confirmed_against_hwinfo() {
        assert_eq!(amd_celsius(0x5B9B_0000, 0.0), 42.5);
        assert_eq!(amd_celsius(0x567B_0000, 0.0), 37.375);
    }

    #[test]
    fn amd_adjustment_applies_for_either_flag() {
        let reading = 400 << 21; // 50 C before any adjustment
        assert_eq!(amd_celsius(reading, 0.0), 50.0);
        assert_eq!(amd_celsius(reading | 0x8_0000, 0.0), 1.0);
        assert_eq!(amd_celsius(reading | 0x3_0000, 0.0), 1.0);
        assert_eq!(amd_celsius(reading | 0x1_0000, 0.0), 50.0);
    }

    #[test]
    fn only_early_amd_parts_have_a_tctl_offset() {
        assert_eq!(amd_tctl_offset("AMD Ryzen 7 1700X Eight-Core Processor"), -20.0);
        assert_eq!(amd_tctl_offset("AMD Ryzen 7 2700X Eight-Core Processor"), -10.0);
        assert_eq!(amd_tctl_offset("AMD Ryzen Threadripper 2950X"), -27.0);
        assert_eq!(amd_tctl_offset("AMD Ryzen 5 5600X 6-Core Processor"), 0.0);
    }

    #[test]
    fn intel_temperature_counts_down_from_tj_max() {
        assert_eq!(intel_celsius(40 << 16, 100 << 16), Some(60.0));
        assert_eq!(intel_celsius(40 << 16, 0), None);
    }
}
