//! "Lighting" page: global brightness, sync and effect of each RGB zone.
//! The effect editor and the helpers below are shared with the device detail page.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, Hsla, IntoElement,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px,
};
use hub_core::SensorKind;
use hub_core::config::TempSource;
use hub_core::rgb::{DEFAULT_EFFECT, Effect, Rgb, RgbConfig, RgbDeviceInfo};

use crate::app::{HubApp, Page};
use crate::state::temp_sources;
use crate::widgets::{
    card, card_header, choice, empty_state, field, hex, loading, muted, page_header, section,
    selectable_row, stepper,
};

/// Effect labels and icons, in the index order of [`default_effect`].
const EFFECTS: [(&str, IconName); 7] = [
    ("Off", IconName::LightbulbOff),
    ("Static", IconName::Palette),
    ("Breathing", IconName::HeartPulse),
    ("Rainbow", IconName::Rainbow),
    ("Wave", IconName::WavesHorizontal),
    ("Temperature", IconName::ThermometerSun),
    ("Load", IconName::Activity),
];
/// Quick picks shown at the top of the color picker.
const PRESETS: [u32; 8] = [
    0xffffff, 0xdc2626, 0xf97316, 0xfacc15, 0x16a34a, 0x06b6d4, 0x2563eb, 0x9333ea,
];
/// Rainbow preview colors.
const RAINBOW: [u32; 6] = [0xdc2626, 0xf97316, 0xfacc15, 0x16a34a, 0x2563eb, 0x9333ea];
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
    /// Zone edited on the device detail page; `None` (or a zone of another device) = whole device.
    pub detail_zone: Option<String>,
}

/// Stateful page controls; their value is copied from the draft on each render.
#[derive(Clone)]
pub struct LightingControls {
    brightness: Entity<SliderState>,
    /// Brightness of the device shown on the detail page.
    pub(crate) device_brightness: Entity<SliderState>,
    /// One color per slot (color, cold/hot, low/high...).
    colors: [Entity<ColorPickerState>; 2],
}

impl LightingControls {
    pub fn new(window: &mut Window, cx: &mut Context<HubApp>) -> (Self, Vec<Subscription>) {
        let percent = || SliderState::new().min(0.).max(100.).step(1.);
        let brightness = cx.new(|_| percent());
        let device_brightness = cx.new(|_| percent());
        let colors = [(), ()].map(|_| cx.new(|cx| ColorPickerState::new(window, cx)));
        let mut subscriptions = vec![
            cx.subscribe_in(
                &brightness,
                window,
                |app: &mut HubApp, _, event: &SliderEvent, _, cx| {
                    let SliderEvent::Change(value) = event else {
                        return;
                    };
                    let value = value.start().round() as u8;
                    app.edit_config(cx, |c| c.rgb.brightness = value);
                },
            ),
            cx.subscribe_in(
                &device_brightness,
                window,
                |app: &mut HubApp, _, event: &SliderEvent, _, cx| {
                    let (SliderEvent::Change(value), Some(id)) = (event, app.device_detail.clone())
                    else {
                        return;
                    };
                    let value = value.start().round() as u8;
                    app.edit_config(cx, |c| set_device_brightness(&mut c.rgb, &id, value));
                },
            ),
        ];
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
        (
            Self {
                brightness,
                device_brightness,
                colors,
            },
            subscriptions,
        )
    }
}

/// Key of a zone in `RgbConfig::zones`.
pub(crate) fn zone_key(device: &str, zone: &str) -> String {
    format!("{device}/{zone}")
}

/// Per-device brightness; 100 % is the default and is not stored.
fn set_device_brightness(rgb: &mut RgbConfig, device: &str, value: u8) {
    if value >= 100 {
        rgb.device_brightness.remove(device);
    } else {
        rgb.device_brightness.insert(device.to_owned(), value);
    }
}

/// Icon matching the kind of RGB device.
// shortcut: guessed from the driver id and label, upgrade when RgbDeviceInfo carries a kind.
pub(crate) fn device_icon(device: &RgbDeviceInfo) -> IconName {
    let label = device.label.to_lowercase();
    if device.id.starts_with("corsair-dram") {
        IconName::MemoryStick
    } else if device.id.starts_with("mystic-light") {
        IconName::CircuitBoard
    } else if label.contains("keyboard") {
        IconName::Keyboard
    } else if label.contains("mouse") {
        IconName::Mouse
    } else {
        IconName::Lightbulb
    }
}

fn rgb_of(color: u32) -> Rgb {
    let [_, r, g, b] = color.to_be_bytes();
    Rgb::new(r, g, b)
}

/// Representative colors of an effect, for the preview swatches.
fn preview_colors(effect: &Effect) -> Vec<Rgb> {
    match effect {
        Effect::Firmware | Effect::Rainbow { .. } => RAINBOW.map(rgb_of).to_vec(),
        Effect::Off => vec![Rgb::BLACK],
        Effect::Static { color } | Effect::Breathing { color, .. } => vec![*color],
        Effect::Wave { colors, .. } => colors.clone(),
        Effect::Temperature { cold, hot, .. } => vec![*cold, *hot],
        Effect::Load { low, high, .. } => vec![*low, *high],
    }
}

/// Row of small color squares previewing an effect.
fn swatches(effect: &Effect, cx: &App) -> Div {
    let border = cx.theme().border;
    h_flex()
        .gap_0p5()
        .flex_none()
        .children(preview_colors(effect).into_iter().map(|color| {
            div()
                .size(px(14.))
                .rounded(px(3.))
                .border_1()
                .border_color(border)
                .bg(to_hsla(color))
        }))
}

/// Effect of a zone as shown in lists (missing or firmware = default effect).
pub(crate) fn zone_effect(rgb: &RgbConfig, key: &str) -> Effect {
    current_effect(rgb, Some(key)).unwrap_or_else(|| DEFAULT_EFFECT.clone())
}

/// Clickable zone row: effect preview, label, details and the effect tag.
pub(crate) fn zone_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    details: impl Into<SharedString>,
    effect: &Effect,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    selectable_row(id, active, cx).child(
        h_flex()
            .gap_3()
            .items_center()
            .child(swatches(effect, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_semibold().child(label.into()))
                    .child(muted(details, cx).text_xs()),
            )
            .child(
                Tag::secondary()
                    .outline()
                    .small()
                    .child(EFFECTS[effect_index(effect)].0),
            ),
    )
}

/// Default effect of each [`EFFECTS`] entry.
fn default_effect(index: usize) -> Effect {
    match index {
        0 => Effect::Off,
        1 => Effect::Static { color: BLUE },
        2 => Effect::Breathing {
            color: BLUE,
            period_ms: 3000,
        },
        3 => Effect::Rainbow { period_ms: 5000 },
        4 => Effect::Wave {
            colors: vec![BLUE, PURPLE],
            period_ms: 4000,
        },
        5 => Effect::Temperature {
            source: TempSource::Sensor {
                id: "cpu/package_temp".into(),
            },
            cold: BLUE,
            hot: RED,
            min_temp: 40.0,
            max_temp: 80.0,
        },
        6 => Effect::Load {
            sensor: "cpu/load".into(),
            low: GREEN,
            high: RED,
        },
        _ => DEFAULT_EFFECT.clone(),
    }
}

fn effect_index(effect: &Effect) -> usize {
    match effect {
        Effect::Off => 0,
        Effect::Static { .. } => 1,
        Effect::Breathing { .. } => 2,
        // Old configs: firmware is shown as the default effect.
        Effect::Rainbow { .. } | Effect::Firmware => 3,
        Effect::Wave { .. } => 4,
        Effect::Temperature { .. } => 5,
        Effect::Load { .. } => 6,
    }
}

/// Adjustable colors of an effect (at most two), with their label.
pub(crate) fn colors(effect: &Effect) -> Vec<(&'static str, Rgb)> {
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

/// "3.0 s".
fn format_period(period_ms: u32) -> String {
    format!("{:.1} s", period_ms as f32 / 1000.0)
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

/// Edited effect: the sync effect if any, otherwise the zone one (missing or firmware = default).
pub(crate) fn current_effect(rgb: &RgbConfig, zone: Option<&str>) -> Option<Effect> {
    let effect = match &rgb.sync {
        Some(effect) => effect.clone(),
        None => rgb
            .zones
            .get(zone?)
            .cloned()
            .unwrap_or_else(|| DEFAULT_EFFECT.clone()),
    };
    Some(if effect == Effect::Firmware {
        DEFAULT_EFFECT.clone()
    } else {
        effect
    })
}

/// Edits the current effect: the sync one, else the first zone's effect copied to every
/// zone of `zones` (a missing zone starts from the default effect).
fn edit_rgb(rgb: &mut RgbConfig, zones: &[String], edit: impl FnOnce(&mut Effect)) {
    if let Some(effect) = rgb.sync.as_mut() {
        edit(effect);
    } else if let Some(mut effect) = current_effect(rgb, zones.first().map(String::as_str)) {
        edit(&mut effect);
        for zone in zones {
            rgb.zones.insert(zone.clone(), effect.clone());
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

/// Copies a value into a slider (no effect if already up to date).
pub(crate) fn sync_slider(
    slider: &Entity<SliderState>,
    value: u8,
    window: &mut Window,
    cx: &mut Context<HubApp>,
) {
    slider.update(cx, |slider, cx| {
        if slider.value().start().round() as u8 != value {
            slider.set_value(f32::from(value), window, cx);
        }
    });
}

/// Copies the draft into the stateful controls (no effect if already up to date).
pub(crate) fn sync_controls(
    controls: &LightingControls,
    brightness: u8,
    colors: &[(&str, Rgb)],
    window: &mut Window,
    cx: &mut Context<HubApp>,
) {
    sync_slider(&controls.brightness, brightness, window, cx);
    for ((_, color), picker) in colors.iter().zip(&controls.colors) {
        picker.update(cx, |picker, cx| {
            if picker.value().map(from_hsla) != Some(*color) {
                picker.set_value(to_hsla(*color), window, cx);
            }
        });
    }
}

/// Slider with its "NN %" value.
pub(crate) fn percent_slider(slider: &Entity<SliderState>, value: u8) -> Div {
    h_flex()
        .gap_3()
        .child(div().w(px(260.)).child(Slider::new(slider)))
        .child(div().w(px(48.)).text_sm().child(format!("{value} %")))
}

impl HubApp {
    /// Zones edited by the effect editor: the device detail target, else the selected zone.
    pub(crate) fn edited_zones(&self) -> Vec<String> {
        let detail = self
            .device_detail
            .as_ref()
            .filter(|_| self.page == Page::Devices);
        let Some(id) = detail else {
            return self.lighting.selected.iter().cloned().collect();
        };
        let keys: Vec<String> = self
            .state
            .rgb_devices
            .iter()
            .filter(|d| &d.id == id)
            .flat_map(|d| d.zones.iter().map(|z| zone_key(&d.id, &z.id)))
            .collect();
        match &self.lighting.detail_zone {
            Some(zone) if keys.contains(zone) => vec![zone.clone()],
            _ => keys,
        }
    }

    fn edit_effect(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Effect)) {
        let zones = self.edited_zones();
        self.edit_config(cx, |c| edit_rgb(&mut c.rgb, &zones, edit));
    }

    /// Opens the detail page of an RGB device.
    pub(crate) fn open_device(&mut self, id: String, cx: &mut Context<Self>) {
        self.page = Page::Devices;
        self.device_detail = Some(id);
        self.lighting.detail_zone = None;
        cx.notify();
    }

    pub fn render_lighting(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let header = page_header(
            IconName::Lightbulb,
            "Lighting",
            "Effects and colors of your RGB devices",
            cx,
        );
        let (Some(draft), Some(controls)) = (
            self.state.draft.as_ref(),
            self.controls.as_ref().map(|c| c.lighting.clone()),
        ) else {
            return loading("Waiting for the service configuration...", cx);
        };
        let rgb = draft.rgb.clone();
        let page = v_flex().gap_4().max_w(px(760.)).child(header);
        if self.state.rgb_devices.is_empty() {
            return page
                .child(empty_state(
                    IconName::LightbulbOff,
                    "No RGB device detected",
                    "Close vendor software such as MSI Center, then restart the service.",
                    cx,
                ))
                .into_any_element();
        }
        // Released devices (see the Devices page) are left to their firmware: hidden here.
        let devices: Vec<_> = self
            .state
            .rgb_devices
            .iter()
            .filter(|d| !rgb.disabled.contains(&d.id))
            .cloned()
            .collect();
        if devices.is_empty() {
            let open = Button::new("open-devices")
                .outline()
                .small()
                .icon(IconName::Microchip)
                .label("Open Devices")
                .on_click(cx.listener(|app, _, _, cx| {
                    app.page = Page::Devices;
                    app.device_detail = None;
                    cx.notify();
                }));
            return page
                .child(empty_state(
                    IconName::Unplug,
                    "All devices are released",
                    "Turn control back on in Devices to set their lighting here.",
                    cx,
                ))
                .child(h_flex().justify_center().child(open))
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

        let general = section(IconName::SunDim, "General", cx)
            .child(field(
                "Brightness",
                percent_slider(&controls.brightness, rgb.brightness),
                cx,
            ))
            .child(field(
                "Same effect on all devices",
                Switch::new("rgb-sync")
                    .checked(rgb.sync.is_some())
                    .on_click(cx.listener(|app, checked: &bool, _, cx| {
                        let sync = checked.then(|| DEFAULT_EFFECT.clone());
                        app.edit_config(cx, |c| c.rgb.sync = sync);
                    })),
                cx,
            ));

        let mut page = page.child(general);
        let mut title = "Effect: all devices".to_owned();
        if rgb.sync.is_none() {
            let mut index = 0usize;
            for (i, device) in devices.iter().enumerate() {
                let id = device.id.clone();
                let configure = Button::new(("rgb-open-device", i))
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .label("Device page")
                    .on_click(cx.listener(move |app, _, _, cx| app.open_device(id.clone(), cx)));
                let mut list = card(cx).child(
                    h_flex()
                        .justify_between()
                        .child(card_header(device_icon(device), device.label.clone(), cx))
                        .child(configure),
                );
                for zone in &device.zones {
                    let key = zone_key(&device.id, &zone.id);
                    let selected = self.lighting.selected.as_ref() == Some(&key);
                    if selected {
                        title = format!("Effect: {}, {}", device.label, zone.label);
                    }
                    let effect = zone_effect(&rgb, &key);
                    let row = zone_row(
                        ("rgb-zone", index),
                        zone.label.clone(),
                        format!("{} LEDs", zone.leds),
                        &effect,
                        selected,
                        cx,
                    )
                    .on_click(cx.listener(move |app, _, _, cx| {
                        app.lighting.selected = Some(key.clone());
                        cx.notify();
                    }));
                    list = list.child(row);
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

    /// Effect picker and its settings (colors, cycle time, source...).
    pub(crate) fn effect_editor(
        &self,
        title: String,
        effect: &Effect,
        controls: &LightingControls,
        cx: &mut Context<Self>,
    ) -> Div {
        let current = effect_index(effect);
        let kinds = EFFECTS.iter().enumerate().map(|(i, (label, icon))| {
            choice(("rgb-effect", i), *label, i == current)
                .icon(*icon)
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.edit_effect(cx, |e| {
                        if effect_index(e) != i {
                            *e = default_effect(i);
                        }
                    });
                }))
        });
        let mut editor = section(IconName::Palette, title, cx).child(
            h_flex()
                .flex_wrap()
                .gap_2()
                .children(kinds.collect::<Vec<_>>()),
        );
        let presets: Vec<Hsla> = PRESETS.iter().map(|c| hex(*c)).collect();
        for (slot, (label, _)) in colors(effect).into_iter().enumerate() {
            editor = editor.child(field(
                label,
                ColorPicker::new(&controls.colors[slot])
                    .featured_colors(presets.clone())
                    .small(),
                cx,
            ));
        }
        if let Some(period_ms) = period(effect) {
            editor = editor.child(field(
                "Cycle time (lower is faster)",
                stepper(
                    "rgb-period",
                    format_period(period_ms),
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

    fn keys(zones: &[&str]) -> Vec<String> {
        zones.iter().map(|z| (*z).to_owned()).collect()
    }

    #[test]
    fn default_effects_match_labels_and_validate() {
        for (index, (label, _)) in EFFECTS.iter().enumerate() {
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
    fn zone_edits_start_from_default_effect() {
        let mut rgb = RgbConfig::default();
        let zone = Some("mystic-light/1462:7e06/jrgb1");
        let zones = keys(&["mystic-light/1462:7e06/jrgb1"]);
        assert_eq!(current_effect(&rgb, zone), Some(DEFAULT_EFFECT.clone()));
        assert_eq!(current_effect(&rgb, None), None);
        edit_rgb(&mut rgb, &[], |e| *e = default_effect(1));
        assert!(rgb.zones.is_empty(), "no zone: nothing edited");
        edit_rgb(&mut rgb, &zones, |e| *e = default_effect(1));
        assert_eq!(rgb.zones.len(), 1);
        edit_rgb(&mut rgb, &zones, |e| set_color(e, 0, RED));
        assert_eq!(
            current_effect(&rgb, zone),
            Some(Effect::Static { color: RED })
        );
        // Old config: an explicit firmware zone is shown and edited as the default effect.
        rgb.zones.insert("d/old".into(), Effect::Firmware);
        assert_eq!(
            current_effect(&rgb, Some("d/old")),
            Some(DEFAULT_EFFECT.clone())
        );
        // Sync mode: only `sync` is changed.
        rgb.sync = Some(Effect::Off);
        edit_rgb(&mut rgb, &zones, |e| *e = default_effect(3));
        assert_eq!(rgb.sync, Some(Effect::Rainbow { period_ms: 5000 }));
        assert_eq!(rgb.zones.len(), 2);
    }

    #[test]
    fn whole_device_edit_copies_first_zone_effect_to_all_zones() {
        let mut rgb = RgbConfig::default();
        rgb.zones
            .insert("d/a".into(), Effect::Static { color: BLUE });
        rgb.zones.insert("d/b".into(), Effect::Off);
        edit_rgb(&mut rgb, &keys(&["d/a", "d/b"]), |e| set_color(e, 0, RED));
        assert_eq!(rgb.zones["d/a"], Effect::Static { color: RED });
        assert_eq!(rgb.zones["d/b"], Effect::Static { color: RED });
    }

    #[test]
    fn device_brightness_default_is_not_stored() {
        let mut rgb = RgbConfig::default();
        set_device_brightness(&mut rgb, "d", 40);
        assert_eq!(rgb.device_brightness["d"], 40);
        set_device_brightness(&mut rgb, "d", 100);
        assert!(rgb.device_brightness.is_empty());
    }

    #[test]
    fn colors_slots_and_steps() {
        let mut wave = default_effect(4);
        set_color(&mut wave, 1, GREEN);
        set_color(&mut wave, 2, RED);
        assert_eq!(colors(&wave), vec![("Color 1", BLUE), ("Color 2", GREEN)]);
        let mut temp = default_effect(5);
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
        let mut rainbow = default_effect(3);
        for _ in 0..20 {
            step_period(&mut rainbow, false);
        }
        assert_eq!(period(&rainbow), Some(PERIOD_STEP));
        assert_eq!(format_period(3000), "3.0 s");
        assert_eq!(rgb_of(0x2563eb), BLUE);
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
