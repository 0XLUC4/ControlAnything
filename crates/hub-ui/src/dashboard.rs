//! "Dashboard" page: cards, temperature history and other readings.

use std::collections::{HashMap, VecDeque};

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, App, IntoElement, ParentElement as _, Styled as _, div, px};
use hub_core::ipc::Snapshot;
use hub_core::{Device, Reading, SensorKind};

use crate::prefs::TempUnit;
use crate::state::{HubState, channel_rpm, display_name};
use crate::widgets::{self, card_header, duty_bar, hex, muted};
use crate::{cards, chart};

/// History points kept per sensor (2 min at 1 Hz).
pub const HISTORY_LEN: usize = 120;

#[derive(Default)]
pub struct Dashboard {
    /// Temperature and load history, by `Reading::id`.
    history: HashMap<String, VecDeque<f32>>,
}

impl Dashboard {
    pub fn apply(&mut self, snapshot: &Snapshot) {
        for reading in &snapshot.readings {
            if matches!(reading.kind, SensorKind::Temperature | SensorKind::Load) {
                let series = self.history.entry(reading.id.clone()).or_default();
                push_capped(series, reading.value, HISTORY_LEN);
            }
        }
    }

    pub fn render(&self, state: &HubState, unit: TempUnit, cx: &App) -> AnyElement {
        let readings = state.readings();
        let (cards, mut shown_ids) = cards::cards_row(readings, unit, cx);
        let cpu_temp = find_kind(readings, Device::Cpu, SensorKind::Temperature);
        let chart = chart::temp_chart(
            cpu_temp.and_then(|r| self.history.get(&r.id)),
            self.history.get(cards::GPU_TEMP),
            unit,
            cx,
        );
        let (fans, fan_ids) = fans_card(state, cx);
        let board_temps: Vec<&Reading> = readings
            .iter()
            .filter(|r| r.device == Device::Motherboard && r.kind == SensorKind::Temperature)
            .collect();
        shown_ids.extend(fan_ids);
        shown_ids.extend(board_temps.iter().map(|r| r.id.clone()));
        v_flex()
            .gap_4()
            .when(!state.connected(), |body| body.child(disconnected_hint(cx)))
            .child(cards)
            .child(
                h_flex()
                    .gap_4()
                    .items_stretch()
                    .child(fans)
                    .child(board_card(&board_temps, unit, cx)),
            )
            .child(chart)
            .children(other_readings(readings, &shown_ids, unit, cx))
            .into_any_element()
    }
}

/// "Fans" card: connected channels, speed and target.
/// Also returns the ids of the RPM sensors shown.
fn fans_card(state: &HubState, cx: &App) -> (AnyElement, Vec<String>) {
    let theme = cx.theme();
    let connected: Vec<_> = state
        .channels
        .iter()
        .filter(|c| state.is_connected(c))
        .collect();
    let shown = connected
        .iter()
        .filter_map(|c| c.rpm_sensor.clone())
        .collect();
    let mut card = widgets::card(cx)
        .flex_1()
        .min_w_0()
        .child(card_header(IconName::Fan, "Fans", cx));
    if connected.is_empty() {
        card = card.child(muted("No connected fan detected.", cx));
    }
    for channel in connected {
        let rpm = channel_rpm(channel, state.readings())
            .map_or("- RPM".into(), |rpm| format!("{rpm:.0} RPM"));
        let duty = state.applied_duty(&channel.id).flatten();
        let mode = duty.map_or("Auto (BIOS)".into(), |d| format!("{d} %"));
        card = card.child(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(display_name(state.config.as_ref(), channel)),
                        )
                        .child(div().flex_none().font_semibold().child(rpm)),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(duty_bar(duty, cx)))
                        .child(
                            div()
                                .w(px(72.))
                                .flex_none()
                                .text_right()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(mode),
                        ),
                ),
        );
    }
    (card.into_any_element(), shown)
}

/// "Motherboard" card: board temperature probes (socket, VRM, chipset, M.2...).
fn board_card(temps: &[&Reading], unit: TempUnit, cx: &App) -> impl IntoElement {
    let mut card = widgets::card(cx)
        .flex_1()
        .min_w_0()
        .child(card_header(IconName::CircuitBoard, "Motherboard", cx));
    if temps.is_empty() {
        card = card.child(muted(
            "Probes unavailable: the service must run as administrator.",
            cx,
        ));
    }
    card.child(v_flex().gap_1().children(temps.iter().map(|r| {
        h_flex()
            .justify_between()
            .text_sm()
            .child(
                h_flex()
                    .gap_2()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::Thermometer).xsmall())
                    .child(r.label.clone()),
            )
            .child(
                div()
                    .font_semibold()
                    .text_color(hex(cards::temp_color(r.value)))
                    .child(unit.format(r.value, 0)),
            )
    })))
}

/// Help banner shown when the daemon does not respond.
fn disconnected_hint(cx: &App) -> impl IntoElement {
    div()
        .p_3()
        .rounded(px(widgets::RADIUS))
        .bg(cx.theme().warning.opacity(0.15))
        .text_sm()
        .text_color(cx.theme().warning)
        .child("ControlAnything service unreachable: start controlanythingd (as administrator for CPU temperature and motherboard fans).")
}

/// Card of the other readings, grouped by component; `None` if none are left.
fn other_readings(
    readings: &[Reading],
    shown_ids: &[String],
    unit: TempUnit,
    cx: &App,
) -> Option<AnyElement> {
    let theme = cx.theme();
    let groups = [
        (Device::Cpu, "Processor"),
        (Device::Gpu, "Graphics card"),
        (Device::Motherboard, "Motherboard"),
        (Device::Memory, "Memory"),
        (Device::Storage, "Storage"),
    ];
    let mut table = widgets::card(cx).child(card_header(IconName::Gauge, "Other sensors", cx));
    let mut any = false;
    for (device, title) in groups {
        let rows: Vec<&Reading> = readings
            .iter()
            .filter(|r| r.device == device && !shown_ids.contains(&r.id))
            .collect();
        if rows.is_empty() {
            continue;
        }
        any = true;
        table = table.child(
            v_flex()
                .child(
                    div()
                        .pb_1()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.muted_foreground)
                        .child(title.to_uppercase()),
                )
                .children(rows.into_iter().map(|r| {
                    h_flex()
                        .justify_between()
                        .py_1()
                        .text_sm()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(div().text_color(theme.muted_foreground).child(r.label.clone()))
                        .child(format_value(Some(r.value), r.kind, unit))
                })),
        );
    }
    any.then(|| table.into_any_element())
}

/// Value formatted with its unit, "-" if the sensor is missing.
/// Temperatures (received in °C) are shown in the chosen unit.
pub fn format_value(value: Option<f32>, kind: SensorKind, unit: TempUnit) -> String {
    match value {
        None => "-".into(),
        Some(v) if kind == SensorKind::Temperature => unit.format(v, 1),
        Some(v) if kind == SensorKind::Power => format!("{v:.1} {}", kind.unit()),
        Some(v) => format!("{v:.0} {}", kind.unit()),
    }
}

/// Appends a value keeping at most `cap` points (the most recent ones).
pub fn push_capped(series: &mut VecDeque<f32>, value: f32, cap: usize) {
    series.push_back(value);
    while series.len() > cap {
        series.pop_front();
    }
}

pub fn find<'a>(readings: &'a [Reading], id: &str) -> Option<&'a Reading> {
    readings.iter().find(|r| r.id == id)
}

/// First sensor of a given component and kind (ids vary by OS).
pub fn find_kind(readings: &[Reading], device: Device, kind: SensorKind) -> Option<&Reading> {
    readings
        .iter()
        .find(|r| r.device == device && r.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(id: &str, device: Device, kind: SensorKind) -> Reading {
        Reading {
            id: id.into(),
            label: id.into(),
            device,
            kind,
            value: 1.0,
        }
    }

    #[test]
    fn history_is_capped_to_most_recent() {
        let mut series = VecDeque::new();
        for v in 0..5 {
            push_capped(&mut series, v as f32, 3);
        }
        assert_eq!(series, VecDeque::from([2.0, 3.0, 4.0]));
    }

    #[test]
    fn finds_readings_by_id_and_kind() {
        let readings = [
            reading("cpu/load", Device::Cpu, SensorKind::Load),
            reading("cpu/package_temp", Device::Cpu, SensorKind::Temperature),
        ];
        assert_eq!(
            find(&readings, "cpu/load").map(|r| r.kind),
            Some(SensorKind::Load)
        );
        assert!(find(&readings, "gpu0/temp").is_none());
        let temp = find_kind(&readings, Device::Cpu, SensorKind::Temperature);
        assert_eq!(temp.map(|r| r.id.as_str()), Some("cpu/package_temp"));
    }

    #[test]
    fn missing_value_shows_dash() {
        let c = TempUnit::Celsius;
        assert_eq!(format_value(None, SensorKind::Temperature, c), "-");
        assert_eq!(
            format_value(Some(51.24), SensorKind::Temperature, c),
            "51.2 °C"
        );
        assert_eq!(
            format_value(Some(50.0), SensorKind::Temperature, TempUnit::Fahrenheit),
            "122.0 °F"
        );
        assert_eq!(format_value(Some(42.6), SensorKind::Load, c), "43 %");
    }
}
