//! Moteur de régulation : courbes, hystérésis, lissage, failsafe. Pur et testable.

use std::time::{Duration, Instant};

use crate::config::{CurvePoint, FailsafeConfig, FanMode, FanSettings, TempSource};
use crate::{Device, Reading, SensorKind};

/// Interpolation linéaire de la courbe ; bornée par le premier et le dernier point.
/// Courbe vide → 100 % (sécurité).
pub fn evaluate_curve(points: &[CurvePoint], temp: f32) -> u8 {
    // Courbe vide ou température NaN → 100 % par sécurité.
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
        // Premier segment [a, b] qui contient temp (existe : first.temp < temp < last.temp).
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

/// Température de la source à partir des mesures ; `None` si un capteur requis manque.
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

/// État de régulation d'un ventilo entre deux ticks.
#[derive(Debug, Default)]
pub struct FanControl {
    /// Température retenue par l'hystérésis.
    retained_temp: Option<f32>,
    /// Dernière consigne émise.
    last_duty: Option<u8>,
    /// Instant du dernier changement de consigne.
    last_change: Option<Instant>,
}

impl FanControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consigne à appliquer. `temp` = température de la source (`None` = capteur perdu).
    /// Retourne `None` pour rendre le canal au firmware (mode Auto ou capteur perdu).
    /// Applique : mode Fixed/Curve, hystérésis, step up/down, temps de réponse, plancher, zéro RPM.
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
        // Capteur perdu (ou NaN) : on rend la main au firmware et on repart de zéro.
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
        self.set_duty(duty, now);
        Some(duty)
    }

    /// Mémorise la consigne ; l'horloge du temps de réponse ne repart que si elle change.
    fn set_duty(&mut self, duty: u8, now: Instant) {
        if self.last_duty != Some(duty) {
            self.last_duty = Some(duty);
            self.last_change = Some(now);
        }
    }
}

/// Avance de `from` vers `to` d'au plus `step` (0 = illimité).
fn step_toward(from: u8, to: u8, step: u8) -> u8 {
    if step == 0 {
        to
    } else if to > from {
        to.min(from.saturating_add(step))
    } else {
        to.max(from.saturating_sub(step))
    }
}

/// Détecteur de surchauffe avec hystérésis de sortie (5 °C sous le seuil).
#[derive(Debug, Default)]
pub struct Failsafe {
    active: bool,
}

impl Failsafe {
    /// Vrai tant qu'une température CPU/GPU est critique : tous les ventilos pilotés à 100 %.
    pub fn update(&mut self, cfg: &FailsafeConfig, readings: &[Reading]) -> bool {
        // (température, seuil critique) des capteurs surveillés.
        let mut watched = readings
            .iter()
            .filter(|r| r.kind == SensorKind::Temperature)
            .filter_map(|r| match r.device {
                Device::Cpu => Some((r.value, cfg.cpu_critical)),
                Device::Gpu => Some((r.value, cfg.gpu_critical)),
                _ => None,
            });
        self.active = if self.active {
            // NaN ne valide pas la sortie : on reste en sécurité.
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

    /// Courbe 30 °C → 20 %, 70 °C → 100 %, sans lissage : chaque test active ce qu'il teste.
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
        assert_eq!(evaluate_curve(&p, 1.0), 3); // 3,33
        assert_eq!(evaluate_curve(&p, 2.0), 7); // 6,67
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
        // Première consigne : saut direct.
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
        // Retour à 0 limité par step_down.
        s.step_down = 20;
        assert_eq!(fc.update(&s, Some(35.0), now), Some(40));
        assert_eq!(fc.update(&s, Some(35.0), now), Some(20));
        assert_eq!(fc.update(&s, Some(35.0), now), Some(0));
        // Reprise au-dessus du seuil : courbe (40 %) et plancher respectés.
        assert_eq!(fc.update(&s, Some(40.0), now), Some(40));
        assert_eq!(fc.update(&s, Some(35.0), now), Some(20));
        s.zero_rpm_below = None;
        assert_eq!(fc.update(&s, Some(31.0), now), Some(25));
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
