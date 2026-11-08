//! Control engine: curves, hysteresis, smoothing, failsafe. Pure and testable.

use std::time::{Duration, Instant};

use crate::config::{CurvePoint, DutyRange, FailsafeConfig, FanMode, FanSettings, TempSource};
use crate::{Device, Reading, SensorKind};

/// Linear interpolation of the curve, clamped by the first and last points.
/// Empty curve gives 100 % (safety).
pub fn evaluate_curve(points: &[CurvePoint], temp: f32) -> u8 {
    // Empty curve or NaN temperature: 100 % for safety.
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return 100;
    };
    if temp.is_nan() {
        return 100;
    }
    let duty = if temp <= first.temp {
        f32::from(first.duty)
    } else if temp >= last.temp {
        f32::from(last.duty)
    } else {
        // First segment [a, b] containing temp (exists: first.temp < temp < last.temp).
        points
            .windows(2)
            .find(|w| temp <= w[1].temp)
            .map_or(f32::from(last.duty), |w| {
                let (a, b) = (w[0], w[1]);
                let ratio = (temp - a.temp) / (b.temp - a.temp);
                f32::from(a.duty) + ratio * (f32::from(b.duty) - f32::from(a.duty))
            })
    };
    duty.round().clamp(0.0, 100.0) as u8
}

/// Source temperature from the readings; `None` if a required sensor is missing.
pub fn resolve_source(source: &TempSource, readings: &[Reading]) -> Option<f32> {
    let value = |id: &String| {
        readings
            .iter()
            .find(|r| &r.id == id)
            .map(|r| r.value)
            .filter(|v| !v.is_nan())
    };
    let values = |ids: &[String]| -> Option<Vec<f32>> {
        if ids.is_empty() {
            return None;
        }
        ids.iter().map(value).collect()
    };
    match source {
        TempSource::Sensor { id } => value(id),
        TempSource::Max { ids } => values(ids)?.into_iter().reduce(f32::max),
        TempSource::Average { ids } => {
            let temps = values(ids)?;
            Some(temps.iter().sum::<f32>() / temps.len() as f32)
        }
    }
}

/// Control state of a fan between two ticks.
#[derive(Debug, Default)]
pub struct FanControl {
    /// Temperature retained by the hysteresis.
    retained_temp: Option<f32>,
    /// Last emitted target.
    last_duty: Option<u8>,
    /// Time of the last target change.
    last_change: Option<Instant>,
}

impl FanControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Target to apply. `temp` = source temperature (`None` = sensor lost).
    /// Returns `None` to hand the channel back to the firmware (Auto mode or sensor lost).
    /// Applies: Fixed/Curve mode, hysteresis, step up/down, response time, floor, zero RPM.
    pub fn update(
        &mut self,
        settings: &FanSettings,
        temp: Option<f32>,
        now: Instant,
    ) -> Option<u8> {
        let points = match &settings.mode {
            FanMode::Auto => {
                *self = Self::default();
                return None;
            }
            FanMode::Fixed { duty } => {
                let duty = (*duty).max(settings.min_duty).min(100);
                self.retained_temp = None;
                self.set_duty(duty, now);
                return Some(duty);
            }
            FanMode::Curve { points, .. } => points,
        };
        // Sensor lost (or NaN): hand control back to the firmware and start over.
        let Some(temp) = temp.filter(|t| t.is_finite()) else {
            *self = Self::default();
            return None;
        };
        let retained = match self.retained_temp {
            Some(r) if (temp - r).abs() < settings.hysteresis => r,
            _ => temp,
        };
        self.retained_temp = Some(retained);

        let target = match settings.zero_rpm_below {
            Some(below) if retained < below => 0,
            _ => evaluate_curve(points, retained)
                .max(settings.min_duty)
                .min(100),
        };
        // Target in the avoided band: stay on the edge we come from (no flip-flop between edges).
        let target = settings.avoid.map_or(target, |band| {
            let high = match self.last_duty {
                Some(last) if last >= band.max => true,
                Some(last) if last <= band.min => false,
                _ => u16::from(target) * 2 >= u16::from(band.min) + u16::from(band.max),
            };
            skip_band(target, band, high || band.min < settings.min_duty)
        });
        let Some(last) = self.last_duty else {
            self.set_duty(target, now);
            return Some(target);
        };
        let response = Duration::from_millis(settings.response_ms);
        if self
            .last_change
            .is_some_and(|c| now.saturating_duration_since(c) < response)
        {
            return Some(last);
        }
        let duty = if target > last {
            step_toward(last, target, settings.step_up)
        } else {
            step_toward(last, target, settings.step_down)
        };
        // A step landing in the band crosses it at once, toward the target (already outside).
        let duty = settings
            .avoid
            .map_or(duty, |band| skip_band(duty, band, duty > last));
        self.set_duty(duty, now);
        Some(duty)
    }

    /// Stores the target; the response-time clock restarts only if it changes.
    fn set_duty(&mut self, duty: u8, now: Instant) {
        if self.last_duty != Some(duty) {
            self.last_duty = Some(duty);
            self.last_change = Some(now);
        }
    }
}

/// Moves from `from` toward `to` by at most `step` (0 = unlimited).
fn step_toward(from: u8, to: u8, step: u8) -> u8 {
    if step == 0 {
        to
    } else if to > from {
        to.min(from.saturating_add(step))
    } else {
        to.max(from.saturating_sub(step))
    }
}

/// Duty strictly inside `band` goes to its upper (`high`) or lower edge; else unchanged.
fn skip_band(duty: u8, band: DutyRange, high: bool) -> u8 {
    match (band.min < duty && duty < band.max, high) {
        (false, _) => duty,
        (true, true) => band.max,
        (true, false) => band.min,
    }
}

/// Overheat detector with exit hysteresis (5 °C below the threshold).
#[derive(Debug, Default)]
pub struct Failsafe {
    active: bool,
}

impl Failsafe {
    /// True while a CPU/GPU temperature is critical: every controlled fan at 100 %.
    pub fn update(&mut self, cfg: &FailsafeConfig, readings: &[Reading]) -> bool {
        // (temperature, critical threshold) of the watched sensors.
        let mut watched = readings
            .iter()
            .filter(|r| r.kind == SensorKind::Temperature)
            .filter_map(|r| match r.device {
                Device::Cpu => Some((r.value, cfg.cpu_critical)),
                Device::Gpu => Some((r.value, cfg.gpu_critical)),
                _ => None,
            });
        self.active = if self.active {
            // NaN does not allow exiting: stay in safe mode.
            !watched.all(|(v, crit)| v < crit - 5.0)
        } else {
            watched.any(|(v, crit)| v >= crit)
        };
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(list: &[(f32, u8)]) -> Vec<CurvePoint> {
        list.iter()
            .map(|&(temp, duty)| CurvePoint { temp, duty })
            .collect()
    }

    fn reading(id: &str, device: Device, kind: SensorKind, value: f32) -> Reading {
        Reading {
            id: id.into(),
            label: id.into(),
            device,
            kind,
            value,
        }
    }

    fn temp(id: &str, value: f32) -> Reading {
        reading(id, Device::Cpu, SensorKind::Temperature, value)
    }

    /// Curve 30 °C to 20 %, 70 °C to 100 %, no smoothing: each test enables what it tests.
    fn curve(min_duty: u8) -> FanSettings {
        FanSettings {
            mode: FanMode::Curve {
                source: TempSource::Sensor { id: "cpu".into() },
                points: pts(&[(30.0, 20), (70.0, 100)]),
            },
            min_duty,
            zero_rpm_below: None,
            hysteresis: 0.0,
            step_up: 0,
            step_down: 0,
            response_ms: 0,
            avoid: None,
        }
    }

    #[test]
    fn curve_interpolation_and_edges() {
        let p = pts(&[(30.0, 20), (50.0, 40), (70.0, 100)]);
        assert_eq!(evaluate_curve(&p, 10.0), 20);
        assert_eq!(evaluate_curve(&p, 30.0), 20);
        assert_eq!(evaluate_curve(&p, 40.0), 30);
        assert_eq!(evaluate_curve(&p, 50.0), 40);
        assert_eq!(evaluate_curve(&p, 60.0), 70);
        assert_eq!(evaluate_curve(&p, 70.0), 100);
        assert_eq!(evaluate_curve(&p, 120.0), 100);
    }

    #[test]
    fn curve_empty_single_and_rounding() {
        assert_eq!(evaluate_curve(&[], 40.0), 100);
        assert_eq!(evaluate_curve(&pts(&[(50.0, 42)]), 10.0), 42);
        assert_eq!(evaluate_curve(&pts(&[(50.0, 42)]), 90.0), 42);
        let p = pts(&[(0.0, 0), (3.0, 10)]);
        assert_eq!(evaluate_curve(&p, 1.0), 3); // 3.33
        assert_eq!(evaluate_curve(&p, 2.0), 7); // 6.67
        assert_eq!(evaluate_curve(&p, f32::NAN), 100);
    }

    #[test]
    fn source_sensor_max_average_missing() {
        let r = [temp("a", 40.0), temp("b", 60.0), temp("n", f32::NAN)];
        let ids = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            resolve_source(&TempSource::Sensor { id: "a".into() }, &r),
            Some(40.0)
        );
        assert_eq!(
            resolve_source(&TempSource::Sensor { id: "x".into() }, &r),
            None
        );
        assert_eq!(
            resolve_source(&TempSource::Sensor { id: "n".into() }, &r),
            None
        );
        assert_eq!(
            resolve_source(
                &TempSource::Max {
                    ids: ids(&["a", "b"])
                },
                &r
            ),
            Some(60.0)
        );
        assert_eq!(
            resolve_source(
                &TempSource::Average {
                    ids: ids(&["a", "b"])
                },
                &r
            ),
            Some(50.0)
        );
        assert_eq!(
            resolve_source(
                &TempSource::Max {
                    ids: ids(&["a", "x"])
                },
                &r
            ),
            None
        );
        assert_eq!(
            resolve_source(
                &TempSource::Average {
                    ids: ids(&["a", "n"])
                },
                &r
            ),
            None
        );
        assert_eq!(resolve_source(&TempSource::Max { ids: vec![] }, &r), None);
        assert_eq!(
            resolve_source(&TempSource::Average { ids: vec![] }, &r),
            None
        );
    }

    #[test]
    fn auto_and_sensor_lost_return_none() {
        let now = Instant::now();
        let mut fc = FanControl::new();
        assert_eq!(fc.update(&FanSettings::default(), Some(50.0), now), None);
        let s = curve(0);
        assert_eq!(fc.update(&s, Some(50.0), now), Some(60));
        assert_eq!(fc.update(&s, None, now), None);
        assert_eq!(fc.update(&s, Some(f32::NAN), now), None);
    }

    #[test]
    fn fixed_applies_min_duty_immediately() {
        let now = Instant::now();
        let mut fc = FanControl::new();
        let mut s = curve(30);
        s.step_up = 1;
        s.response_ms = 10_000;
        s.mode = FanMode::Fixed { duty: 10 };
        assert_eq!(fc.update(&s, None, now), Some(30));
        s.mode = FanMode::Fixed { duty: 80 };
        assert_eq!(fc.update(&s, None, now), Some(80));
        s.mode = FanMode::Fixed { duty: 200 };
        assert_eq!(fc.update(&s, None, now), Some(100));
    }

    #[test]
    fn hysteresis_holds_then_moves() {
        let now = Instant::now();
        let mut fc = FanControl::new();
        let mut s = curve(0);
        s.hysteresis = 2.0;
        assert_eq!(fc.update(&s, Some(50.0), now), Some(60));
        assert_eq!(fc.update(&s, Some(51.9), now), Some(60));
        assert_eq!(fc.update(&s, Some(48.5), now), Some(60));
        assert_eq!(fc.update(&s, Some(52.0), now), Some(64));
        assert_eq!(fc.update(&s, Some(50.5), now), Some(64));
        assert_eq!(fc.update(&s, Some(49.5), now), Some(59));
    }

    #[test]
    fn step_up_and_down_limit() {
        let now = Instant::now();
        let mut fc = FanControl::new();
        let mut s = curve(0);
        s.step_up = 10;
        s.step_down = 5;
        // First target: direct jump.
        assert_eq!(fc.update(&s, Some(30.0), now), Some(20));
        assert_eq!(fc.update(&s, Some(70.0), now), Some(30));
        assert_eq!(fc.update(&s, Some(70.0), now), Some(40));
        assert_eq!(fc.update(&s, Some(30.0), now), Some(35));
        assert_eq!(fc.update(&s, Some(30.0), now), Some(30));
        assert_eq!(fc.update(&s, Some(30.0), now), Some(25));
        assert_eq!(fc.update(&s, Some(30.0), now), Some(20));
        assert_eq!(fc.update(&s, Some(30.0), now), Some(20));
    }

    #[test]
    fn response_time_holds_last_duty() {
        let t0 = Instant::now();
        let mut fc = FanControl::new();
        let mut s = curve(0);
        s.response_ms = 1000;
        assert_eq!(fc.update(&s, Some(30.0), t0), Some(20));
        assert_eq!(
            fc.update(&s, Some(70.0), t0 + Duration::from_millis(500)),
            Some(20)
        );
        assert_eq!(
            fc.update(&s, Some(70.0), t0 + Duration::from_millis(1000)),
            Some(100)
        );
        assert_eq!(
            fc.update(&s, Some(30.0), t0 + Duration::from_millis(1500)),
            Some(100)
        );
        assert_eq!(
            fc.update(&s, Some(30.0), t0 + Duration::from_millis(2000)),
            Some(20)
        );
    }

    #[test]
    fn min_duty_floor() {
        let now = Instant::now();
        let mut fc = FanControl::new();
        let s = curve(35);
        assert_eq!(fc.update(&s, Some(20.0), now), Some(35));
        assert_eq!(fc.update(&s, Some(70.0), now), Some(100));
    }

    #[test]
    fn zero_rpm_below_threshold_then_resume() {
        let now = Instant::now();
        let mut fc = FanControl::new();
        let mut s = curve(25);
        s.zero_rpm_below = Some(40.0);
        assert_eq!(fc.update(&s, Some(35.0), now), Some(0));
        assert_eq!(fc.update(&s, Some(50.0), now), Some(60));
        // Return to 0 limited by step_down.
        s.step_down = 20;
        assert_eq!(fc.update(&s, Some(35.0), now), Some(40));
        assert_eq!(fc.update(&s, Some(35.0), now), Some(20));
        assert_eq!(fc.update(&s, Some(35.0), now), Some(0));
        // Resume above the threshold: curve (40 %) and floor respected.
        assert_eq!(fc.update(&s, Some(40.0), now), Some(40));
        assert_eq!(fc.update(&s, Some(35.0), now), Some(20));
        s.zero_rpm_below = None;
        assert_eq!(fc.update(&s, Some(31.0), now), Some(25));
    }

    #[test]
    fn avoid_band_jumps_to_edges_without_oscillation() {
        let now = Instant::now();
        let mut fc = FanControl::new();
        let mut s = curve(0); // 30 °C 20 %, 70 °C 100 %: 2 % per °C
        s.avoid = Some(DutyRange { min: 40, max: 60 });
        // First target 50 (tie): upper edge; 44 (nearest): lower edge.
        assert_eq!(fc.update(&s, Some(45.0), now), Some(60));
        let mut low = FanControl::new();
        assert_eq!(low.update(&s, Some(42.0), now), Some(40));
        // Coming from above, a target inside the band holds the upper edge.
        assert_eq!(fc.update(&s, Some(42.0), now), Some(60));
        assert_eq!(fc.update(&s, Some(42.0), now), Some(60));
        // Coming from below, it holds the lower edge.
        assert_eq!(low.update(&s, Some(48.0), now), Some(40));
        // Edges and outside values are kept.
        assert_eq!(low.update(&s, Some(40.0), now), Some(40));
        assert_eq!(low.update(&s, Some(30.0), now), Some(20));
        assert_eq!(low.update(&s, Some(50.0), now), Some(60));
        // Steps crossing the band jump over it, toward the target.
        let mut fc = FanControl::new();
        s.step_up = 10;
        s.step_down = 10;
        assert_eq!(fc.update(&s, Some(40.0), now), Some(40));
        assert_eq!(fc.update(&s, Some(70.0), now), Some(60));
        assert_eq!(fc.update(&s, Some(70.0), now), Some(70));
        assert_eq!(fc.update(&s, Some(30.0), now), Some(60));
        assert_eq!(fc.update(&s, Some(30.0), now), Some(40));
        assert_eq!(fc.update(&s, Some(30.0), now), Some(30));
        // Lower edge under the floor: the upper edge is used.
        let mut fc = FanControl::new();
        let mut s = curve(45);
        s.avoid = Some(DutyRange { min: 40, max: 60 });
        assert_eq!(fc.update(&s, Some(30.0), now), Some(60));
    }

    fn failsafe_readings(cpu: f32, gpu: f32) -> Vec<Reading> {
        vec![
            temp("cpu/package", cpu),
            reading("gpu/core", Device::Gpu, SensorKind::Temperature, gpu),
        ]
    }

    #[test]
    fn failsafe_triggers_on_cpu_and_gpu() {
        let cfg = FailsafeConfig::default(); // CPU 90, GPU 85
        let mut fs = Failsafe::default();
        assert!(!fs.update(&cfg, &failsafe_readings(89.9, 84.9)));
        assert!(fs.update(&cfg, &failsafe_readings(90.0, 50.0)));
        let mut fs = Failsafe::default();
        assert!(fs.update(&cfg, &failsafe_readings(50.0, 85.0)));
    }

    #[test]
    fn failsafe_stays_until_all_below_critical_minus_5() {
        let cfg = FailsafeConfig::default();
        let mut fs = Failsafe::default();
        assert!(fs.update(&cfg, &failsafe_readings(95.0, 50.0)));
        assert!(fs.update(&cfg, &failsafe_readings(85.0, 50.0)));
        assert!(fs.update(&cfg, &failsafe_readings(84.0, 80.0)));
        assert!(!fs.update(&cfg, &failsafe_readings(84.0, 79.9)));
        assert!(!fs.update(&cfg, &failsafe_readings(89.0, 84.0)));
    }

    #[test]
    fn failsafe_ignores_other_kinds_and_devices() {
        let cfg = FailsafeConfig::default();
        let mut fs = Failsafe::default();
        let r = [
            reading("cpu/power", Device::Cpu, SensorKind::Power, 200.0),
            reading("gpu/clock", Device::Gpu, SensorKind::Clock, 2000.0),
            reading(
                "mb/temp",
                Device::Motherboard,
                SensorKind::Temperature,
                120.0,
            ),
            reading("nvme", Device::Storage, SensorKind::Temperature, 120.0),
        ];
        assert!(!fs.update(&cfg, &r));
    }
}
