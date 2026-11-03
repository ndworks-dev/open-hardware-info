//! Contract between the `ohi-sensor` service and the widget, in shared memory.
//! The temperature is one 64-bit word, written by the service and read-only for
//! everyone else. A single atomic word cannot be read half-updated, so neither
//! side ever blocks. The GPU fan curve has its own mapping, described below.
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

// ---------------------------------------------------------------- GPU fans

/// Unlike the temperature, this mapping is writable by interactive users: the
/// widget publishes the curve here. The service treats every word as untrusted,
/// and the worst a local program can do is ask for a valid curve, which the
/// service still overrides at `EMERGENCY_ON_C`.
pub const FAN_MAPPING_NAME: &str = r"Global\OpenHardwareInfo.FanControl.v1";
pub const FAN_MAPPING_SIZE: u32 = 40;
pub const FAN_WORD_LEASE: usize = 0;
pub const FAN_WORD_CURVE: usize = 1; // and the next one
pub const FAN_WORD_STATUS: usize = 3;
pub const FAN_WORD_MIN_SPEED: usize = 4;

pub const MIN_CURVE_POINTS: usize = 2;
pub const MAX_CURVE_POINTS: usize = 8;
pub const CURVE_TEMP_MIN: u8 = 20;
pub const CURVE_TEMP_MAX: u8 = 89;

/// How long a curve stays valid without the widget renewing it. If the widget
/// closes or crashes, the card goes back to its own automatic control.
const LEASE_MS: u64 = 10_000;
/// Lowering the speed waits for this drop, so the fans do not hunt around a point.
const HYSTERESIS_C: u8 = 3;
/// At this temperature the fans go to 100% whatever the curve says, until the
/// card cools down to `EMERGENCY_OFF_C`.
const EMERGENCY_ON_C: u8 = 90;
const EMERGENCY_OFF_C: u8 = 85;

/// `[temperature in C, fan percent]`, by rising temperature. Below the first
/// point the card decides, which keeps its fan stop at low temperatures.
pub type CurvePoint = [u8; 2];

pub fn curve_error(points: &[CurvePoint]) -> Option<String> {
    if !(MIN_CURVE_POINTS..=MAX_CURVE_POINTS).contains(&points.len()) {
        return Some(format!("a curva de fans precisa de {MIN_CURVE_POINTS} a {MAX_CURVE_POINTS} pontos"));
    }
    if points.iter().any(|[temp, _]| !(CURVE_TEMP_MIN..=CURVE_TEMP_MAX).contains(temp)) {
        return Some(format!("a temperatura da curva de fans deve ficar entre {CURVE_TEMP_MIN} e {CURVE_TEMP_MAX} °C"));
    }
    if points.iter().any(|[_, speed]| *speed > 100) {
        return Some("a velocidade da curva de fans deve ficar entre 0 e 100%".to_owned());
    }
    if points.windows(2).any(|pair| pair[1][0] <= pair[0][0] || pair[1][1] < pair[0][1]) {
        return Some("na curva de fans, a temperatura deve subir a cada ponto e a velocidade não pode cair".to_owned());
    }
    None
}

/// Two words hold up to `MAX_CURVE_POINTS`, and a temperature of 0 ends the list.
pub fn encode_curve(points: &[CurvePoint]) -> [u64; 2] {
    let mut bytes = [0u8; 16];
    for (slot, point) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(points) {
        *slot = *point;
    }
    let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = bytes;
    [u64::from_le_bytes([a, b, c, d, e, f, g, h]), u64::from_le_bytes([i, j, k, l, m, n, o, p])]
}

/// The words change one at a time and anyone may write them, so a torn or hostile
/// value is possible. It only gets through if it is still a valid curve.
pub fn decode_curve(words: [u64; 2]) -> Option<Vec<CurvePoint>> {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&words[0].to_le_bytes());
    bytes[8..].copy_from_slice(&words[1].to_le_bytes());
    let points: Vec<CurvePoint> = bytes.as_chunks::<2>().0.iter().copied().take_while(|[temp, _]| *temp != 0).collect();
    curve_error(&points).is_none().then_some(points)
}

pub fn lease_is_fresh(lease: u64, now_ms: u64) -> bool {
    lease != 0 && lease <= now_ms.saturating_add(1_000) && now_ms.saturating_sub(lease) <= LEASE_MS
}

/// The fan percent the curve asks for, where 0 means "let the card decide".
pub fn curve_speed(points: &[CurvePoint], temp: u8) -> u8 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else { return 0 };
    if temp < first[0] {
        return 0;
    }
    if temp >= last[0] {
        return last[1];
    }
    let upper = points.iter().position(|point| point[0] > temp).unwrap_or(points.len() - 1);
    let (a, b) = (points[upper - 1], points[upper]);
    let (span, rise, done) = (u32::from(b[0] - a[0]), u32::from(b[1] - a[1]), u32::from(temp - a[0]));
    a[1] + ((rise * done + span / 2) / span) as u8
}

/// Decides the speed one reading at a time, remembering what it last asked for.
#[derive(Default)]
pub struct FanController {
    speed: u8,
    emergency: bool,
}

impl FanController {
    pub fn is_emergency(&self) -> bool {
        self.emergency
    }

    /// The percent to apply, where 0 means "let the card decide". Without a curve
    /// the card is left alone, because its own control is then in charge.
    pub fn step(&mut self, curve: Option<&[CurvePoint]>, temp: u8) -> u8 {
        let Some(points) = curve else {
            *self = Self::default();
            return 0;
        };
        if temp >= EMERGENCY_ON_C {
            self.emergency = true;
        } else if temp < EMERGENCY_OFF_C {
            self.emergency = false;
        }
        if self.emergency {
            self.speed = 100;
            return 100;
        }
        let rising = curve_speed(points, temp);
        let falling = curve_speed(points, temp.saturating_add(HYSTERESIS_C));
        if rising > self.speed {
            self.speed = rising;
        } else if falling < self.speed {
            self.speed = falling;
        }
        self.speed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FanState {
    /// No curve in effect: the card controls its own fans.
    Automatic,
    Curve,
    Emergency,
    /// No usable GPU, or it refused the change. The card is in automatic control.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FanStatus {
    pub state: FanState,
    pub temp: u8,
    pub speed: u8,
}

/// Bits 63..24 hold the `GetTickCount64` time, and the rest the state, the
/// temperature and the fan percent.
pub fn encode_status(status: FanStatus, tick_ms: u64) -> u64 {
    let state = match status.state {
        FanState::Automatic => 0u64,
        FanState::Curve => 1,
        FanState::Emergency => 2,
        FanState::Failed => 3,
    };
    (tick_ms << 24) | (state << 16) | (u64::from(status.temp) << 8) | u64::from(status.speed)
}

pub fn decode_status(word: u64, now_ms: u64) -> Option<FanStatus> {
    if word == 0 || now_ms.saturating_sub(word >> 24) > MAX_AGE_MS {
        return None;
    }
    let state = match (word >> 16) & 0xFF {
        0 => FanState::Automatic,
        1 => FanState::Curve,
        2 => FanState::Emergency,
        3 => FanState::Failed,
        _ => return None,
    };
    Some(FanStatus { state, temp: (word >> 8) as u8, speed: word as u8 })
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

    const CURVE: [CurvePoint; 3] = [[50, 30], [70, 50], [85, 100]];

    #[test]
    fn curves_survive_the_round_trip() {
        assert_eq!(decode_curve(encode_curve(&CURVE)), Some(CURVE.to_vec()));
        let full: Vec<CurvePoint> = (0..MAX_CURVE_POINTS as u8).map(|i| [40 + i, 30 + i]).collect();
        assert_eq!(decode_curve(encode_curve(&full)), Some(full));
    }

    #[test]
    fn invalid_curves_are_rejected() {
        assert!(curve_error(&CURVE).is_none());
        assert!(curve_error(&[[50, 30]]).is_some());
        assert!(curve_error(&[[50, 30], [50, 40]]).is_some());
        assert!(curve_error(&[[50, 40], [60, 30]]).is_some());
        assert!(curve_error(&[[50, 30], [60, 101]]).is_some());
        assert!(curve_error(&[[10, 30], [60, 40]]).is_some());
        assert!(curve_error(&[[50, 30], [90, 40]]).is_some());
        assert_eq!(decode_curve([0, 0]), None);
        assert_eq!(decode_curve([u64::MAX, u64::MAX]), None);
    }

    #[test]
    fn leases_expire_and_cannot_come_from_the_future() {
        assert!(lease_is_fresh(1_000, 1_000 + LEASE_MS));
        assert!(!lease_is_fresh(1_000, 1_001 + LEASE_MS));
        assert!(!lease_is_fresh(0, 1_000));
        assert!(!lease_is_fresh(u64::MAX, 1_000));
    }

    #[test]
    fn speed_interpolates_between_points() {
        assert_eq!(curve_speed(&CURVE, 49), 0);
        assert_eq!(curve_speed(&CURVE, 50), 30);
        assert_eq!(curve_speed(&CURVE, 60), 40);
        assert_eq!(curve_speed(&CURVE, 70), 50);
        assert_eq!(curve_speed(&CURVE, 85), 100);
        assert_eq!(curve_speed(&CURVE, 89), 100);
    }

    #[test]
    fn speed_only_falls_after_the_temperature_drops_further() {
        let mut fan = FanController::default();
        assert_eq!(fan.step(Some(&CURVE), 60), 40);
        assert_eq!(fan.step(Some(&CURVE), 58), 40);
        assert_eq!(fan.step(Some(&CURVE), 56), 39);
    }

    #[test]
    fn the_card_takes_over_again_below_the_first_point() {
        let mut fan = FanController::default();
        assert_eq!(fan.step(Some(&CURVE), 50), 30);
        assert_eq!(fan.step(Some(&CURVE), 47), 30);
        assert_eq!(fan.step(Some(&CURVE), 46), 0);
    }

    #[test]
    fn emergency_overrides_any_curve_until_the_card_cools_down() {
        let quiet = [[50, 30], [89, 30]];
        let mut fan = FanController::default();
        assert_eq!(fan.step(Some(&quiet), 89), 30);
        assert_eq!(fan.step(Some(&quiet), 90), 100);
        assert!(fan.is_emergency());
        assert_eq!(fan.step(Some(&quiet), 85), 100);
        assert_eq!(fan.step(Some(&quiet), 84), 30);
        assert!(!fan.is_emergency());
    }

    #[test]
    fn losing_the_curve_hands_control_back_at_any_temperature() {
        let mut fan = FanController::default();
        assert_eq!(fan.step(Some(&CURVE), 95), 100);
        assert_eq!(fan.step(None, 95), 0);
        assert!(!fan.is_emergency());
    }

    #[test]
    fn status_survives_the_round_trip_and_goes_stale() {
        let status = FanStatus { state: FanState::Curve, temp: 61, speed: 45 };
        assert_eq!(decode_status(encode_status(status, 1_000), 1_500), Some(status));
        assert_eq!(decode_status(encode_status(status, 1_000), 1_001 + MAX_AGE_MS), None);
        assert_eq!(decode_status(0, 1_000), None);
    }
}
