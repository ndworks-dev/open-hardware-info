//! Contract between the `ohi-sensor` service and the widget: one 64-bit word in
//! shared memory, written by the service and read-only for everyone else. A
//! single atomic word cannot be read half-updated, so neither side ever blocks.
//!
//! The name carries the protocol version. Changing the layout means a new name,
//! so a widget update never has to replace the service, which needs elevation.

pub const MAPPING_NAME: &str = r"Global\OpenHardwareInfo.CpuTemp.v1";
pub const MAPPING_SIZE: u32 = 8;
pub const SERVICE_NAME: &str = "OpenHardwareInfoSensor";

/// The service publishes every second; anything older means it stopped or stalled.
const MAX_AGE_MS: u64 = 5_000;

pub fn is_plausible(celsius: f64) -> bool {
    (-40.0..=150.0).contains(&celsius)
}

/// Bits 63..16 hold the `GetTickCount64` time of the reading, and bits 15..0 the
/// temperature in eighths of a degree, the sensor's own resolution.
pub fn encode(celsius: f64, tick_ms: u64) -> u64 {
    let eighths = (celsius * 8.0).round() as i16;
    (tick_ms << 16) | u64::from(eighths as u16)
}

pub fn decode(word: u64, now_ms: u64) -> Option<f64> {
    if word == 0 || now_ms.saturating_sub(word >> 16) > MAX_AGE_MS {
        return None;
    }
    let celsius = f64::from(word as u16 as i16) / 8.0;
    is_plausible(celsius).then_some(celsius)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_survive_the_round_trip() {
        assert_eq!(decode(encode(42.5, 1_000), 1_500), Some(42.5));
        assert_eq!(decode(encode(-5.25, 1_000), 1_500), Some(-5.25));
    }

    #[test]
    fn empty_or_stale_readings_are_rejected() {
        assert_eq!(decode(0, 1_000), None);
        assert_eq!(decode(encode(42.5, 1_000), 1_000 + MAX_AGE_MS), Some(42.5));
        assert_eq!(decode(encode(42.5, 1_000), 1_001 + MAX_AGE_MS), None);
    }

    #[test]
    fn impossible_temperatures_are_rejected() {
        assert_eq!(decode(encode(200.0, 1_000), 1_000), None);
        assert_eq!(decode(encode(-100.0, 1_000), 1_000), None);
    }
}
