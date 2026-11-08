//! "Lighting" page: global brightness, sync and effect of each RGB zone.

use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, Hsla, IntoElement, ParentElement as _,
    Styled as _, Subscription, Window, div, px,
};
use hub_core::SensorKind;
use hub_core::config::TempSource;
use hub_core::rgb::{Effect, Rgb, RgbConfig};

use crate::app::HubApp;
use crate::fans::waiting;
use crate::state::temp_sources;
use crate::widgets::{choice, field, hex, muted, section, stepper};

/// Effect labels, in the index order of [`default_effect`].
const EFFECTS: [&str; 8] = [
    "Firmware default",
    "Off",
    "Static",
    "Breathing",
    "Rainbow",
    "Wave",
    "Temperature",
    "Load",
];
const BLUE: Rgb = Rgb::new(0x25, 0x63, 0xeb);
const PURPLE: Rgb = Rgb::new(0x93, 0x33, 0xea);
const GREEN: Rgb = Rgb::new(0x16, 0xa3, 0x4a);
const RED: Rgb = Rgb::new(0xdc, 0x26, 0x26);
const PERIOD_STEP: u32 = 500;
const PERIOD_MAX: u32 = 60_000;
/// Temperature threshold step and minimum gap between min and max (°C).
const TEMP_STEP: f32 = 5.0;
const TEMP_MAX: f32 = 110.0;

/// Page state that outlives the window.
#[derive(Default)]
pub struct LightingPage {
    /// Edited zone, key `"{device_id}/{zone_id}"` (ignored in sync mode).
    pub selected: Option<String>,
}

/// Stateful page controls; their value is copied from the draft on each render.
#[derive(Clone)]
pub struct LightingControls {
    brightness: Entity<SliderState>,
    /// One color per slot (color, cold/hot, low/high...).
    colors: [Entity<ColorPickerState>; 2],
}

impl LightingControls {
    pub fn new(window: &mut Window, cx: &mut Context<HubApp>) -> (Self, Vec<Subscription>) {
        let brightness = cx.new(|_| SliderState::new().min(0.).max(100.).step(1.));
        let colors = [(), ()].map(|_| cx.new(|cx| ColorPickerState::new(window, cx)));
        let mut subscriptions = vec![cx.subscribe_in(
            &brightness,
            window,
            |app: &mut HubApp, _, event: &SliderEvent, _, cx| {
                let SliderEvent::Change(value) = event else {
                    return;
                };
                let value = value.start().round() as u8;
                app.edit_config(cx, |c| c.rgb.brightness = value);
            },
        )];
        for (slot, picker) in colors.iter().enumerate() {
            subscriptions.push(cx.subscribe_in(
                picker,
                window,
                move |app: &mut HubApp, _, event: &ColorPickerEvent, _, cx| {
                    if let ColorPickerEvent::Change(Some(color)) = event {
                        let color = from_hsla(*color);
                        app.edit_effect(cx, |e| set_color(e, slot, color));
                    }
                },
            ));
        }
        (Self { brightness, colors }, subscriptions)
    }
}

/// Key of a zone in `RgbConfig::zones`.
fn zone_key(device: &str, zone: &str) -> String {
    format!("{device}/{zone}")
}

/// Default effect of each [`EFFECTS`] entry.
fn default_effect(index: usize) -> Effect {
    match index {
        1 => Effect::Off,
        2 => Effect::Static { color: BLUE },
        3 => Effect::Breathing {
            color: BLUE,
            period_ms: 3000,
        },
        4 => Effect::Rainbow { period_ms: 5000 },
        5 => Effect::Wave {
            colors: vec![BLUE, PURPLE],
            period_ms: 4000,
        },
        6 => Effect::Temperature {
            source: TempSource::Sensor {
                id: "cpu/package_temp".into(),
            },
            cold: BLUE,
            hot: RED,
            min_temp: 40.0,
            max_temp: 80.0,
        },
        7 => Effect::Load {
            sensor: "cpu/load".into(),
            low: GREEN,
            high: RED,
        },
        _ => Effect::Firmware,
    }
}

fn effect_index(effect: &Effect) -> usize {
    match effect {
        Effect::Firmware => 0,
        Effect::Off => 1,
        Effect::Static { .. } => 2,
        Effect::Breathing { .. } => 3,
        Effect::Rainbow { .. } => 4,
        Effect::Wave { .. } => 5,
        Effect::Temperature { .. } => 6,
        Effect::Load { .. } => 7,
    }
}

/// Adjustable colors of an effect (at most two), with their label.
fn colors(effect: &Effect) -> Vec<(&'static str, Rgb)> {
    match effect {
        Effect::Static { color } | Effect::Breathing { color, .. } => vec![("Color", *color)],
        Effect::Wave { colors, .. } => ["Color 1", "Color 2"]
            .into_iter()
            .zip(colors.iter().copied())
            .collect(),
        Effect::Temperature { cold, hot, .. } => {
            vec![("Cold color", *cold), ("Hot color", *hot)]
        }
        Effect::Load { low, high, .. } => vec![("Low load", *low), ("High load", *high)],
        _ => Vec::new(),
    }
}

/// Replaces the color of slot `slot` (see [`colors`]).
fn set_color(effect: &mut Effect, slot: usize, rgb: Rgb) {
    let target = match (effect, slot) {
        (Effect::Static { color } | Effect::Breathing { color, .. }, 0) => color,
        (Effect::Wave { colors, .. }, slot) => match colors.get_mut(slot) {
            Some(color) => color,
            None => return,
        },
        (Effect::Temperature { cold, .. }, 0) => cold,
        (Effect::Temperature { hot, .. }, 1) => hot,
        (Effect::Load { low, .. }, 0) => low,
        (Effect::Load { high, .. }, 1) => high,
        _ => return,
    };
    *target = rgb;
}

fn period(effect: &Effect) -> Option<u32> {
    match effect {
        Effect::Breathing { period_ms, .. }
        | Effect::Rainbow { period_ms }
        | Effect::Wave { period_ms, .. } => Some(*period_ms),
        _ => None,
    }
}

/// Lengthens or shortens the animation period by one step.
fn step_period(effect: &mut Effect, up: bool) {
    if let Effect::Breathing { period_ms, .. }
    | Effect::Rainbow { period_ms }
    | Effect::Wave { period_ms, .. } = effect
    {
        *period_ms = if up {
            (*period_ms + PERIOD_STEP).min(PERIOD_MAX)
        } else {
            period_ms.saturating_sub(PERIOD_STEP).max(PERIOD_STEP)
        };
    }
}

/// Shifts a temperature threshold keeping `min + TEMP_STEP <= max`.
fn step_temp(effect: &mut Effect, max_bound: bool, up: bool) {
    if let Effect::Temperature {
        min_temp, max_temp, ..
    } = effect
    {
        let delta = if up { TEMP_STEP } else { -TEMP_STEP };
        if max_bound {
            *max_temp = (*max_temp + delta).clamp(*min_temp + TEMP_STEP, TEMP_MAX);
        } else {
            *min_temp = (*min_temp + delta).clamp(0.0, *max_temp - TEMP_STEP);
        }
    }
}

/// Edited effect: the sync effect if any, otherwise the zone one (missing = firmware).
fn current_effect(rgb: &RgbConfig, zone: Option<&str>) -> Option<Effect> {
    match &rgb.sync {
        Some(effect) => Some(effect.clone()),
        None => Some(rgb.zones.get(zone?).cloned().unwrap_or(Effect::Firmware)),
    }
}

/// Edits the current effect; a zone back to firmware is removed (missing = firmware).
fn edit_rgb(rgb: &mut RgbConfig, zone: Option<&str>, edit: impl FnOnce(&mut Effect)) {
    if let Some(effect) = rgb.sync.as_mut() {
        edit(effect);
    } else if let Some(zone) = zone {
        let effect = rgb.zones.entry(zone.to_owned()).or_insert(Effect::Firmware);
        edit(effect);
        if *effect == Effect::Firmware {
            rgb.zones.remove(zone);
        }
    }
}

fn to_hsla(color: Rgb) -> Hsla {
    hex(u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b))
}

fn from_hsla(color: Hsla) -> Rgb {
    let color = color.to_rgb();
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb::new(channel(color.r), channel(color.g), channel(color.b))
}

/// Copies the draft into the stateful controls (no effect if already up to date).
fn sync_controls(
    controls: &LightingControls,
    brightness: u8,
    colors: &[(&str, Rgb)],
    window: &mut Window,
    cx: &mut Context<HubApp>,
) {
    controls.brightness.update(cx, |slider, cx| {
        if slider.value().start().round() as u8 != brightness {
            slider.set_value(f32::from(brightness), window, cx);
        }
    });
    for ((_, color), picker) in colors.iter().zip(&controls.colors) {
        picker.update(cx, |picker, cx| {
            if picker.value().map(from_hsla) != Some(*color) {
                picker.set_value(to_hsla(*color), window, cx);
            }
        });
    }
}

impl HubApp {
    fn edit_effect(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Effect)) {
        let zone = self.lighting.selected.clone();
        self.edit_config(cx, |c| edit_rgb(&mut c.rgb, zone.as_deref(), edit));
    }

    pub fn render_lighting(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (Some(draft), Some(controls)) = (
            self.state.draft.as_ref(),
            self.controls.as_ref().map(|c| c.lighting.clone()),
        ) else {
            return waiting(cx);
        };
        let rgb = draft.rgb.clone();
        let devices = self.state.rgb_devices.clone();
        if devices.is_empty() {
            return muted(
                "No RGB device detected (close MSI Center)",
                cx,
            )
            .into_any_element();
        }
        // Edited zone: the first one until an existing zone is chosen.
        let keys: Vec<String> = devices
            .iter()
            .flat_map(|d| d.zones.iter().map(|z| zone_key(&d.id, &z.id)))
            .collect();
        if !self
            .lighting
            .selected
            .as_ref()
            .is_some_and(|k| keys.contains(k))
        {
            self.lighting.selected = keys.into_iter().next();
        }
        let effect = current_effect(&rgb, self.lighting.selected.as_deref());
        let effect_colors = effect.as_ref().map(colors).unwrap_or_default();
        sync_controls(&controls, rgb.brightness, &effect_colors, window, cx);

        let general = section("General", cx)
            .child(field(
                "Brightness",
                h_flex()
                    .gap_3()
                    .child(div().w(px(260.)).child(Slider::new(&controls.brightness)))
                    .child(
                        div()
                            .w(px(48.))
                            .text_sm()
                            .child(format!("{} %", rgb.brightness)),
                    ),
                cx,
            ))
            .child(field(
                "Sync all",
                Switch::new("rgb-sync")
                    .checked(rgb.sync.is_some())
                    .on_click(cx.listener(|app, checked: &bool, _, cx| {
                        let sync = checked.then_some(Effect::Firmware);
                        app.edit_config(cx, |c| c.rgb.sync = sync);
                    })),
                cx,
            ));

        let mut page = v_flex().gap_4().max_w(px(760.)).child(general);
        let mut title = "Synced effect (all devices)".to_owned();
        if rgb.sync.is_none() {
            let mut index = 0usize;
            for device in &devices {
                let mut list = section(device.label.clone(), cx);
                for zone in &device.zones {
                    let key = zone_key(&device.id, &zone.id);
                    let selected = self.lighting.selected.as_ref() == Some(&key);
                    if selected {
                        title = format!("{}: {}", device.label, zone.label);
                    }
                    let current = rgb.zones.get(&key).map_or(0, effect_index);
                    let button = choice(("rgb-zone", index), EFFECTS[current], selected).on_click(
                        cx.listener(move |app, _, _, cx| {
                            app.lighting.selected = Some(key.clone());
                            cx.notify();
                        }),
                    );
                    list = list.child(field(zone.label.clone(), button, cx));
                    index += 1;
                }
                page = page.child(list);
            }
        }
        match effect {
            Some(effect) => page
                .child(self.effect_editor(title, &effect, &controls, cx))
                .into_any_element(),
            None => page.into_any_element(),
        }
    }

    fn effect_editor(
        &self,
        title: String,
        effect: &Effect,
        controls: &LightingControls,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let current = effect_index(effect);
        let kinds = EFFECTS.iter().enumerate().map(|(i, label)| {
            choice(("rgb-effect", i), *label, i == current).on_click(cx.listener(
                move |app, _, _, cx| {
                    app.edit_effect(cx, |e| {
                        if effect_index(e) != i {
                            *e = default_effect(i);
                        }
                    });
                },
            ))
        });
        let mut editor = section(title, cx).child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .children(kinds.collect::<Vec<_>>()),
        );
        if *effect == Effect::Firmware {
            editor = editor.child(muted(
                "The service writes nothing: the original lighting is kept.",
                cx,
            ));
        }
        for (slot, (label, _)) in colors(effect).into_iter().enumerate() {
            editor = editor.child(field(
                label,
                ColorPicker::new(&controls.colors[slot]).small(),
                cx,
            ));
        }
        if let Some(period_ms) = period(effect) {
            editor = editor.child(field(
                "Period",
                stepper(
                    "rgb-period",
                    format!("{period_ms} ms"),
                    cx.listener(|app, _, _, cx| app.edit_effect(cx, |e| step_period(e, false))),
                    cx.listener(|app, _, _, cx| app.edit_effect(cx, |e| step_period(e, true))),
                ),
                cx,
            ));
        }
        match effect {
            Effect::Temperature {
                source,
                min_temp,
                max_temp,
                ..
            } => {
                let unit = self.prefs.temperature_unit;
                let sources = temp_sources(self.state.readings())
                    .into_iter()
                    .enumerate()
                    .map(|(i, (label, item))| {
                        let selected = item == *source;
                        choice(("rgb-temp-source", i), label, selected).on_click(cx.listener(
                            move |app, _, _, cx| {
                                let item = item.clone();
                                app.edit_effect(cx, |e| {
                                    if let Effect::Temperature { source, .. } = e {
                                        *source = item;
                                    }
                                });
                            },
                        ))
                    });
                editor = editor
                    .child(field(
                        "Source",
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .children(sources.collect::<Vec<_>>()),
                        cx,
                    ))
                    .child(field(
                        "Cold temperature",
                        stepper(
                            "rgb-min-temp",
                            unit.format(*min_temp, 0),
                            cx.listener(|app, _, _, cx| {
                                app.edit_effect(cx, |e| step_temp(e, false, false))
                            }),
                            cx.listener(|app, _, _, cx| {
                                app.edit_effect(cx, |e| step_temp(e, false, true))
                            }),
                        ),
                        cx,
                    ))
                    .child(field(
                        "Hot temperature",
                        stepper(
                            "rgb-max-temp",
                            unit.format(*max_temp, 0),
                            cx.listener(|app, _, _, cx| {
                                app.edit_effect(cx, |e| step_temp(e, true, false))
                            }),
                            cx.listener(|app, _, _, cx| {
                                app.edit_effect(cx, |e| step_temp(e, true, true))
                            }),
                        ),
                        cx,
                    ));
            }
            Effect::Load { sensor, .. } => {
                let sensors = self
                    .state
                    .readings()
                    .iter()
                    .filter(|r| r.kind == SensorKind::Load)
                    .enumerate()
                    .map(|(i, reading)| {
                        let id = reading.id.clone();
                        choice(("rgb-load-sensor", i), reading.label.clone(), id == *sensor)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                let id = id.clone();
                                app.edit_effect(cx, |e| {
                                    if let Effect::Load { sensor, .. } = e {
                                        *sensor = id;
                                    }
                                });
                            }))
                    })
                    .collect::<Vec<_>>();
                editor = editor.child(field(
                    "Sensor",
                    h_flex().flex_wrap().gap_2().children(sensors),
                    cx,
                ));
            }
            _ => {}
        }
        editor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_effects_match_labels_and_validate() {
        for (index, label) in EFFECTS.iter().enumerate() {
            let effect = default_effect(index);
            assert_eq!(effect_index(&effect), index);
            let mut config = RgbConfig {
                sync: Some(effect.clone()),
                ..RgbConfig::default()
            };
            assert!(config.validate().is_ok(), "{label}");
            assert_eq!(config.sync, Some(effect), "default already normalized");
        }
    }

    #[test]
    fn zone_edits_drop_firmware_entries() {
        let mut rgb = RgbConfig::default();
        let zone = Some("mystic-light/1462:7e06/jrgb1");
        assert_eq!(current_effect(&rgb, zone), Some(Effect::Firmware));
        assert_eq!(current_effect(&rgb, None), None);
        edit_rgb(&mut rgb, zone, |e| *e = default_effect(2));
        assert_eq!(rgb.zones.len(), 1);
        edit_rgb(&mut rgb, zone, |e| set_color(e, 0, RED));
        assert_eq!(
            current_effect(&rgb, zone),
            Some(Effect::Static { color: RED })
        );
        edit_rgb(&mut rgb, zone, |e| *e = Effect::Firmware);
        assert!(rgb.zones.is_empty());
        // Sync mode: only `sync` is changed.
        rgb.sync = Some(Effect::Off);
        edit_rgb(&mut rgb, zone, |e| *e = default_effect(4));
        assert_eq!(rgb.sync, Some(Effect::Rainbow { period_ms: 5000 }));
        assert!(rgb.zones.is_empty());
    }

    #[test]
    fn colors_slots_and_steps() {
        let mut wave = default_effect(5);
        set_color(&mut wave, 1, GREEN);
        set_color(&mut wave, 2, RED);
        assert_eq!(
            colors(&wave),
            vec![("Color 1", BLUE), ("Color 2", GREEN)]
        );
        let mut temp = default_effect(6);
        set_color(&mut temp, 1, GREEN);
        for _ in 0..20 {
            step_temp(&mut temp, false, true);
        }
        let Effect::Temperature {
            hot,
            min_temp,
            max_temp,
            ..
        } = temp
        else {
            panic!("temperature expected");
        };
        assert_eq!((hot, min_temp, max_temp), (GREEN, 75.0, 80.0));
        let mut rainbow = default_effect(4);
        for _ in 0..20 {
            step_period(&mut rainbow, false);
        }
        assert_eq!(period(&rainbow), Some(PERIOD_STEP));
    }

    #[test]
    fn hsla_roundtrip_is_exact() {
        for color in [
            BLUE,
            PURPLE,
            GREEN,
            RED,
            Rgb::BLACK,
            Rgb::WHITE,
            Rgb::new(1, 2, 3),
        ] {
            assert_eq!(from_hsla(to_hsla(color)), color);
        }
    }
}
