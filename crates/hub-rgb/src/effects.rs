//! Effect engine: computes each zone's frame from the config and the readings.
//! Pure, stateless functions: the daemon and the UI preview use them as-is.

use std::f32::consts::TAU;
use std::time::Duration;

use hub_core::Reading;
use hub_core::control::resolve_source;
use hub_core::rgb::{DEFAULT_EFFECT, Effect, Rgb, RgbConfig};

/// Effective effect of a zone: `sync` if set, else `zones["{device}/{zone}"]`, else [`DEFAULT_EFFECT`].
pub fn effect_for<'a>(cfg: &'a RgbConfig, device_id: &str, zone_id: &str) -> &'a Effect {
    cfg.sync
        .as_ref()
        .or_else(|| cfg.zones.get(&format!("{device_id}/{zone_id}")))
        .unwrap_or(&DEFAULT_EFFECT)
}

/// Frame of a zone of `leds` LEDs at time `elapsed` (since engine start).
/// `None` for `Effect::Firmware` (write nothing). Brightness 0..=100 applied.
///
/// Missing temperature sensor: show `hot`, to be safe (a visible false alarm beats a
/// false "all good"). Missing load sensor: `low`.
pub fn render(
    effect: &Effect,
    leds: usize,
    elapsed: Duration,
    readings: &[Reading],
    brightness: u8,
) -> Option<Vec<Rgb>> {
    // Position along the zone, 0..1 (0 for a single-LED zone).
    let pos = |i: usize| i as f32 / leds as f32;
    let colors: Vec<Rgb> = match effect {
        Effect::Firmware => return None,
        Effect::Off => vec![Rgb::BLACK; leds],
        Effect::Static { color } => vec![*color; leds],
        Effect::Breathing { color, period_ms } => {
            // 0 at t=0, maximum at half period: smoothed sine.
            let level = (1.0 - (TAU * phase(elapsed, *period_ms)).cos()) / 2.0;
            vec![scale(*color, level); leds]
        }
        Effect::Rainbow { period_ms } => {
            let t = phase(elapsed, *period_ms);
            (0..leds).map(|i| hue((t + pos(i)).fract())).collect()
        }
        Effect::Wave { colors, period_ms } => {
            if colors.is_empty() {
                return Some(vec![Rgb::BLACK; leds]);
            }
            let t = phase(elapsed, *period_ms);
            (0..leds)
                .map(|i| gradient(colors, (t + pos(i)).fract()))
                .collect()
        }
        Effect::Temperature {
            source,
            cold,
            hot,
            min_temp,
            max_temp,
        } => {
            let color = match resolve_source(source, readings) {
                Some(temp) => lerp(*cold, *hot, (temp - min_temp) / (max_temp - min_temp)),
                None => *hot,
            };
            vec![color; leds]
        }
        Effect::Load { sensor, low, high } => {
            let load = readings.iter().find(|r| &r.id == sensor).map(|r| r.value);
            let color = load.map_or(*low, |v| lerp(*low, *high, v / 100.0));
            vec![color; leds]
        }
    };
    let level = f32::from(brightness.min(100)) / 100.0;
    Some(colors.into_iter().map(|c| scale(c, level)).collect())
}

/// Progress within the period, 0..1. Period 0 -> 1000 ms.
fn phase(elapsed: Duration, period_ms: u32) -> f32 {
    let period = if period_ms == 0 {
        1000
    } else {
        u128::from(period_ms)
    };
    (elapsed.as_millis() % period) as f32 / period as f32
}

/// Color at position `pos` (0..1) of a cyclic gradient through `colors` (non-empty).
fn gradient(colors: &[Rgb], pos: f32) -> Rgb {
    let x = pos * colors.len() as f32;
    let idx = (x as usize).min(colors.len() - 1);
    lerp(
        colors[idx],
        colors[(idx + 1) % colors.len()],
        x - idx as f32,
    )
}

/// Linear interpolation; `f` clamped to 0..1 (NaN -> `a`).
fn lerp(a: Rgb, b: Rgb, f: f32) -> Rgb {
    let f = if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) };
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * f).round() as u8;
    Rgb::new(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b))
}

/// Multiplies each channel by `level` (0..1).
fn scale(c: Rgb, level: f32) -> Rgb {
    lerp(Rgb::BLACK, c, level)
}

/// Full-saturation, full-value hue; `h` in 0..1.
fn hue(h: f32) -> Rgb {
    let x = h * 6.0;
    let up = ((x.fract()) * 255.0).round() as u8;
    let down = 255 - up;
    match x as u8 {
        0 => Rgb::new(255, up, 0),
        1 => Rgb::new(down, 255, 0),
        2 => Rgb::new(0, 255, up),
        3 => Rgb::new(0, down, 255),
        4 => Rgb::new(up, 0, 255),
        _ => Rgb::new(255, 0, down),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::config::TempSource;
    use hub_core::{Device, SensorKind};

    const RED: Rgb = Rgb::new(255, 0, 0);
    const BLUE: Rgb = Rgb::new(0, 0, 255);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn reading(id: &str, value: f32) -> Reading {
        Reading {
            id: id.into(),
            label: id.into(),
            device: Device::Cpu,
            kind: SensorKind::Temperature,
            value,
        }
    }

    fn one(effect: &Effect, elapsed: Duration, readings: &[Reading]) -> Rgb {
        render(effect, 1, elapsed, readings, 100).expect("image")[0]
    }

    #[test]
    fn firmware_off_static_and_empty_zone() {
        assert_eq!(render(&Effect::Firmware, 4, ms(0), &[], 100), None);
        assert_eq!(
            render(&Effect::Off, 3, ms(0), &[], 100),
            Some(vec![Rgb::BLACK; 3])
        );
        let stat = Effect::Static { color: RED };
        assert_eq!(render(&stat, 2, ms(123), &[], 100), Some(vec![RED; 2]));
        assert_eq!(render(&stat, 0, ms(0), &[], 100), Some(vec![]));
        let rainbow = Effect::Rainbow { period_ms: 1000 };
        assert_eq!(render(&rainbow, 0, ms(0), &[], 100), Some(vec![]));
    }

    #[test]
    fn brightness_scales_and_clamps() {
        let stat = Effect::Static {
            color: Rgb::new(200, 100, 0),
        };
        assert_eq!(
            render(&stat, 1, ms(0), &[], 50),
            Some(vec![Rgb::new(100, 50, 0)])
        );
        assert_eq!(render(&stat, 1, ms(0), &[], 0), Some(vec![Rgb::BLACK]));
        assert_eq!(
            render(&stat, 1, ms(0), &[], 255),
            Some(vec![Rgb::new(200, 100, 0)])
        );
    }

    #[test]
    fn breathing_follows_sine() {
        let effect = Effect::Breathing {
            color: Rgb::WHITE,
            period_ms: 2000,
        };
        assert_eq!(one(&effect, ms(0), &[]), Rgb::BLACK);
        assert_eq!(one(&effect, ms(1000), &[]), Rgb::WHITE);
        assert_eq!(one(&effect, ms(500), &[]), Rgb::new(128, 128, 128));
        assert_eq!(one(&effect, ms(2000), &[]), Rgb::BLACK);
        // Period 0 -> 1000 ms.
        let zero = Effect::Breathing {
            color: Rgb::WHITE,
            period_ms: 0,
        };
        assert_eq!(one(&zero, ms(500), &[]), Rgb::WHITE);
    }

    #[test]
    fn rainbow_cycles_and_spreads() {
        let effect = Effect::Rainbow { period_ms: 1200 };
        assert_eq!(one(&effect, ms(0), &[]), RED);
        assert_eq!(one(&effect, ms(400), &[]), Rgb::new(0, 255, 0));
        assert_eq!(one(&effect, ms(600), &[]), Rgb::new(0, 255, 255));
        assert_eq!(one(&effect, ms(800), &[]), BLUE);
        let frame = render(&effect, 3, ms(0), &[], 100).expect("image");
        assert_eq!(frame, vec![RED, Rgb::new(0, 255, 0), BLUE]);
        // Shifted by a third of a period: each LED takes its neighbour's hue.
        let later = render(&effect, 3, ms(400), &[], 100).expect("image");
        assert_eq!(later, vec![frame[1], frame[2], frame[0]]);
    }

    #[test]
    fn wave_scrolls_through_colors() {
        let effect = Effect::Wave {
            colors: vec![RED, BLUE],
            period_ms: 1000,
        };
        assert_eq!(one(&effect, ms(0), &[]), RED);
        assert_eq!(one(&effect, ms(500), &[]), BLUE);
        assert_eq!(one(&effect, ms(250), &[]), Rgb::new(128, 0, 128));
        assert_eq!(one(&effect, ms(750), &[]), Rgb::new(128, 0, 128));
        let frame = render(&effect, 4, ms(0), &[], 100).expect("image");
        assert_eq!(frame[0], RED);
        assert_eq!(frame[2], BLUE);
        let later = render(&effect, 4, ms(250), &[], 100).expect("image");
        assert_eq!(later, vec![frame[1], frame[2], frame[3], frame[0]]);
        // Empty colors (unvalidated config): black, without panicking.
        let empty = Effect::Wave {
            colors: vec![],
            period_ms: 1000,
        };
        assert_eq!(
            render(&empty, 2, ms(0), &[], 100),
            Some(vec![Rgb::BLACK; 2])
        );
    }

    #[test]
    fn temperature_lerps_clamps_and_shows_hot_when_missing() {
        let effect = Effect::Temperature {
            source: TempSource::Max {
                ids: vec!["cpu".into(), "gpu".into()],
            },
            cold: BLUE,
            hot: RED,
            min_temp: 40.0,
            max_temp: 80.0,
        };
        let at = |t: f32| one(&effect, ms(0), &[reading("cpu", t), reading("gpu", 0.0)]);
        assert_eq!(at(60.0), Rgb::new(128, 0, 128));
        assert_eq!(at(40.0), BLUE);
        assert_eq!(at(10.0), BLUE);
        assert_eq!(at(80.0), RED);
        assert_eq!(at(120.0), RED);
        assert_eq!(one(&effect, ms(0), &[reading("cpu", 50.0)]), RED);
        assert_eq!(one(&effect, ms(0), &[]), RED);
    }

    #[test]
    fn load_lerps_and_defaults_to_low() {
        let effect = Effect::Load {
            sensor: "cpu/load".into(),
            low: BLUE,
            high: RED,
        };
        assert_eq!(one(&effect, ms(0), &[reading("cpu/load", 0.0)]), BLUE);
        assert_eq!(
            one(&effect, ms(0), &[reading("cpu/load", 50.0)]),
            Rgb::new(128, 0, 128)
        );
        assert_eq!(one(&effect, ms(0), &[reading("cpu/load", 100.0)]), RED);
        assert_eq!(one(&effect, ms(0), &[reading("cpu/load", 150.0)]), RED);
        assert_eq!(one(&effect, ms(0), &[reading("cpu/load", f32::NAN)]), BLUE);
        assert_eq!(one(&effect, ms(0), &[]), BLUE);
    }

    #[test]
    fn effect_for_precedence() {
        let mut cfg = RgbConfig::default();
        assert_eq!(effect_for(&cfg, "dev", "z1"), &DEFAULT_EFFECT);
        cfg.zones.insert("dev/z1".into(), Effect::Off);
        assert_eq!(effect_for(&cfg, "dev", "z1"), &Effect::Off);
        assert_eq!(effect_for(&cfg, "dev", "z2"), &DEFAULT_EFFECT);
        cfg.sync = Some(Effect::Static { color: RED });
        assert_eq!(
            effect_for(&cfg, "dev", "z1"),
            &Effect::Static { color: RED }
        );
        assert_eq!(
            effect_for(&cfg, "dev", "z2"),
            &Effect::Static { color: RED }
        );
    }
}
