//! "Dashboard" page: cards, temperature history, alerts and other readings.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::{AnyElement, App, Context, IntoElement, ParentElement as _, Styled as _, div, px};
use hub_core::ipc::Snapshot;
use hub_core::{Device, Reading, SensorKind};

use crate::app::HubApp;
use crate::prefs::TempUnit;
use crate::state::{
    HubState, Notice, channel_rpm, display_name, is_pump, mode_label, settings_for,
};
use crate::widgets::{
    self, card_header, duty_bar, format_rpm, group_label, hex, muted, page_header, section,
    segmented, stat_line, stepper,
};
use crate::{cards, chart};

/// History points kept per sensor: 1 h at the default 1 s poll.
/// shortcut: faster polling (0.5 s) keeps only 30 min; store by time if that matters.
pub const HISTORY_LEN: usize = 3600;
/// Chart time ranges: label, seconds.
const RANGES: [(&str, u64); 3] = [("2 min", 120), ("10 min", 600), ("1 h", 3600)];
/// Alert threshold steppers: (first value, step, min, max).
const TEMP_STEP: (f32, f32, f32, f32) = (80.0, 5.0, 40.0, 110.0);
const RPM_STEP: (f32, f32, f32, f32) = (300.0, 100.0, 100.0, 3000.0);

#[derive(Default)]
pub struct Dashboard {
    /// Temperature and load history, by `Reading::id`, right-aligned on `times`.
    history: HashMap<String, VecDeque<f32>>,
    /// Sensor labels, for the CSV header.
    labels: HashMap<String, String>,
    /// Unix time (s) of each snapshot.
    times: VecDeque<u64>,
    /// Index in `RANGES`.
    range: usize,
    /// Alerts card expanded.
    edit_alerts: bool,
}

impl Dashboard {
    pub fn apply(&mut self, snapshot: &Snapshot) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        push_capped(&mut self.times, now, HISTORY_LEN);
        for reading in &snapshot.readings {
            if matches!(reading.kind, SensorKind::Temperature | SensorKind::Load) {
                let series = self.history.entry(reading.id.clone()).or_default();
                push_capped(series, reading.value, HISTORY_LEN);
                if !self.labels.contains_key(&reading.id) {
                    self.labels
                        .insert(reading.id.clone(), reading.label.clone());
                }
            }
        }
    }

    /// Whole history as CSV: one row per snapshot, one column per sensor (°C, %).
    fn to_csv(&self) -> String {
        let mut ids: Vec<&String> = self.history.keys().collect();
        ids.sort();
        let header: Vec<&str> = ids
            .iter()
            .map(|id| self.labels.get(*id).map_or(id.as_str(), String::as_str))
            .collect();
        let mut text = csv_header(&header);
        let len = self.times.len();
        for (i, &time) in self.times.iter().enumerate() {
            let values: Vec<Option<f32>> = ids
                .iter()
                .map(|id| {
                    let series = &self.history[*id];
                    let offset = len.saturating_sub(series.len());
                    i.checked_sub(offset).and_then(|j| series.get(j).copied())
                })
                .collect();
            text.push_str(&csv_row(time, &values));
        }
        text
    }
}

impl HubApp {
    pub fn render_dashboard(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let unit = self.prefs.temperature_unit;
        let state = &self.state;
        let readings = state.readings();
        let (cards, mut shown_ids) = cards::cards_row(readings, unit, cx);
        let (fans, fan_ids) = fans_card(state, cx);
        let board_temps: Vec<&Reading> = readings
            .iter()
            .filter(|r| r.device == Device::Motherboard && r.kind == SensorKind::Temperature)
            .collect();
        shown_ids.extend(fan_ids);
        shown_ids.extend(board_temps.iter().map(|r| r.id.clone()));
        let offline = (!state.connected()).then(|| {
            Alert::warning(
                "service-offline",
                "Start the ControlAnything service (as administrator for CPU temperature and motherboard fans).",
            )
            .title("Service unreachable")
        });
        let middle = h_flex()
            .gap_4()
            .items_stretch()
            .child(fans)
            .child(board_card(&board_temps, unit, cx));
        let others = other_readings(readings, &shown_ids, unit, cx);
        v_flex()
            .gap_4()
            .child(page_header(
                IconName::LayoutDashboard,
                "Dashboard",
                "Live readings",
                cx,
            ))
            .children(offline)
            .child(cards)
            .child(middle)
            .child(self.chart_card(cx))
            .child(self.alerts_card(cx))
            .children(others)
            .into_any_element()
    }

    /// Temperature history with its range picker and CSV export.
    fn chart_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let readings = self.state.readings();
        let cpu_temp = find_kind(readings, Device::Cpu, SensorKind::Temperature);
        let history = &self.dashboard.history;
        let body = chart::temp_chart(
            cpu_temp.and_then(|r| history.get(&r.id)),
            history.get(cards::GPU_TEMP),
            &self.dashboard.times,
            RANGES[self.dashboard.range].1,
            self.prefs.temperature_unit,
            cx,
        );
        let range = segmented(
            "history-range",
            RANGES.map(|(label, _)| label),
            self.dashboard.range,
        )
        .on_click(cx.listener(|app, clicks: &Vec<usize>, _, cx| {
            if let Some(&index) = clicks.first() {
                app.dashboard.range = index.min(RANGES.len() - 1);
                cx.notify();
            }
        }));
        let export = Button::new("export-history")
            .small()
            .outline()
            .icon(IconName::FileDown)
            .label("Export CSV")
            .tooltip("Save the recorded temperatures and loads (up to 1 h)")
            .disabled(self.dashboard.times.is_empty())
            .on_click(cx.listener(|app, _, _, cx| app.export_history(cx)));
        widgets::card(cx)
            .child(
                h_flex()
                    .justify_between()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(card_header(IconName::ChartLine, "Temperatures", cx))
                            .child(chart::legend(cx)),
                    )
                    .child(h_flex().gap_2().child(range).child(export)),
            )
            .child(body)
            .into_any_element()
    }

    /// Writes the history to a CSV file chosen by the user (Documents by default).
    fn export_history(&mut self, cx: &mut Context<Self>) {
        let text = self.dashboard.to_csv();
        let directory = directories::UserDirs::new()
            .and_then(|dirs| dirs.document_dir().map(Path::to_path_buf))
            .or_else(std::env::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let chosen = cx.prompt_for_new_path(&directory, Some("controlanything-history.csv"));
        cx.spawn(async move |this, cx| {
            let notice = match chosen.await {
                Ok(Ok(Some(path))) => Some(match std::fs::write(&path, text) {
                    Ok(()) => Notice::Info(format!("History exported: {}", path.display())),
                    Err(err) => Notice::Error(format!("Export failed: {err}")),
                }),
                Ok(Err(err)) => Some(Notice::Error(format!("Dialog unavailable: {err}"))),
                // Cancelled by the user.
                Ok(Ok(None)) | Err(_) => None,
            };
            let _ = this.update(cx, |app, cx| {
                if notice.is_some() {
                    app.state.notice = notice;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// "Alerts" card: user thresholds, collapsed to a summary by default.
    fn alerts_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let unit = self.prefs.temperature_unit;
        let count = self.prefs.temp_alarms.len() + self.prefs.rpm_alarms.len();
        let editing = self.dashboard.edit_alerts;
        let summary = match count {
            0 => "None".to_owned(),
            n => format!("{n} active"),
        };
        let toggle = Button::new("edit-alerts")
            .small()
            .ghost()
            .icon(if editing {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .label(if editing { "Done" } else { "Edit" })
            .on_click(cx.listener(|app, _, _, cx| {
                app.dashboard.edit_alerts = !app.dashboard.edit_alerts;
                cx.notify();
            }));
        let mut card = widgets::card(cx).child(
            h_flex()
                .justify_between()
                .child(
                    h_flex()
                        .gap_2()
                        .child(card_header(IconName::Bell, "Alerts", cx))
                        .child(Tag::secondary().outline().small().child(summary)),
                )
                .child(toggle),
        );
        if !editing {
            return card
                .child(muted(
                    "Desktop notification when a temperature goes above, or a fan below, your limit.",
                    cx,
                ))
                .into_any_element();
        }
        let readings = self.state.readings();
        let temps: Vec<&Reading> = readings
            .iter()
            .filter(|r| r.kind == SensorKind::Temperature)
            .collect();
        let fans: Vec<&Reading> = readings
            .iter()
            .filter(|r| r.kind == SensorKind::Fan)
            .collect();
        if temps.is_empty() && fans.is_empty() {
            return card
                .child(muted("No sensor reading yet.", cx))
                .into_any_element();
        }
        if !temps.is_empty() {
            card = card.child(group_label("Temperature above", cx));
            for r in temps {
                let limit = self.prefs.temp_alarms.get(&r.id).copied();
                let text = limit.map_or("Off".into(), |v| unit.format(v, 0));
                card = card.child(alarm_row(r, false, text, cx));
            }
        }
        if !fans.is_empty() {
            card = card.child(group_label("Fan below (stalled)", cx));
            for r in fans {
                let limit = self.prefs.rpm_alarms.get(&r.id).copied();
                let text = limit.map_or("Off".into(), |v| format_rpm(Some(v)));
                card = card.child(alarm_row(r, true, text, cx));
            }
        }
        card.into_any_element()
    }

    /// Raises or lowers one threshold (below the minimum: off), then saves the preferences.
    fn step_alarm(&mut self, id: &str, rpm: bool, up: bool, cx: &mut Context<Self>) {
        let (alarms, steps) = if rpm {
            (&mut self.prefs.rpm_alarms, RPM_STEP)
        } else {
            (&mut self.prefs.temp_alarms, TEMP_STEP)
        };
        match step_threshold(alarms.get(id).copied(), up, steps) {
            Some(value) => alarms.insert(id.to_owned(), value),
            None => alarms.remove(id),
        };
        if let Err(message) = self.prefs.save() {
            self.notify_error(format!("Preferences not saved: {message}"), cx);
        }
        cx.notify();
    }
}

/// Sensor label + "-" / value / "+" stepper for one threshold.
fn alarm_row(
    reading: &Reading,
    rpm: bool,
    text: String,
    cx: &mut Context<HubApp>,
) -> impl IntoElement + use<> {
    let (dec_id, inc_id) = (reading.id.clone(), reading.id.clone());
    let dec = cx.listener(move |app, _, _, cx| app.step_alarm(&dec_id, rpm, false, cx));
    let inc = cx.listener(move |app, _, _, cx| app.step_alarm(&inc_id, rpm, true, cx));
    let prefix = if rpm { "rpm-alarm" } else { "temp-alarm" };
    h_flex()
        .justify_between()
        .gap_4()
        .text_sm()
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .child(reading.label.clone()),
        )
        .child(stepper(&format!("{prefix}-{}", reading.id), text, dec, inc))
}

/// Next threshold value: off → `first`, then by `step` within `min..=max`; below `min` → off.
fn step_threshold(
    value: Option<f32>,
    up: bool,
    (first, step, min, max): (f32, f32, f32, f32),
) -> Option<f32> {
    match value {
        None => up.then_some(first),
        Some(v) if up => Some((v + step).min(max)),
        Some(v) => Some(v - step).filter(|next| *next >= min),
    }
}

/// "Fans" card: connected channels with mode, speed and applied duty.
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
    let mut card = section(IconName::Fan, "Fans", cx).flex_1().min_w_0();
    if connected.is_empty() {
        card = card.child(muted("No connected fan detected.", cx));
    }
    for channel in connected {
        let rpm = format_rpm(channel_rpm(channel, state.readings()));
        let duty = state.applied_duty(&channel.id).flatten();
        let mode = state.config.as_ref().map_or("Auto".into(), |c| {
            mode_label(&settings_for(c, &channel.id).mode)
        });
        let icon = if is_pump(channel) {
            IconName::Droplets
        } else {
            IconName::Fan
        };
        let name_row = h_flex()
            .gap_2()
            .items_center()
            .text_sm()
            .child(Icon::new(icon).small().text_color(theme.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(display_name(state.config.as_ref(), channel)),
            )
            .child(Tag::secondary().outline().small().child(mode))
            .child(
                div()
                    .w(px(84.))
                    .flex_none()
                    .text_right()
                    .font_semibold()
                    .child(rpm),
            );
        let duty_row = h_flex()
            .gap_2()
            .items_center()
            .child(div().flex_1().child(duty_bar(duty, cx)))
            .child(
                div()
                    .w(px(84.))
                    .flex_none()
                    .text_right()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(duty.map_or("BIOS".into(), |d| format!("{d} %"))),
            );
        card = card.child(v_flex().gap_1().child(name_row).child(duty_row));
    }
    (card.into_any_element(), shown)
}

/// "Motherboard" card: board temperature probes (socket, VRM, chipset, M.2...).
fn board_card(temps: &[&Reading], unit: TempUnit, cx: &App) -> impl IntoElement {
    let mut card = section(IconName::CircuitBoard, "Motherboard", cx)
        .flex_1()
        .min_w_0();
    if temps.is_empty() {
        card = card.child(muted(
            "Probes unavailable: the service must run as administrator.",
            cx,
        ));
    }
    card.child(v_flex().gap_2().children(temps.iter().map(|r| {
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

/// Card of the other readings, grouped by component; `None` if none are left.
fn other_readings(
    readings: &[Reading],
    shown_ids: &[String],
    unit: TempUnit,
    cx: &App,
) -> Option<AnyElement> {
    let groups = [
        (Device::Cpu, "Processor"),
        (Device::Gpu, "Graphics card"),
        (Device::Motherboard, "Motherboard"),
        (Device::Memory, "Memory"),
        (Device::Storage, "Storage"),
    ];
    let mut table = section(IconName::Gauge, "Other sensors", cx);
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
        table = table.child(v_flex().gap_1().child(group_label(title, cx)).children(
            rows.into_iter().map(|r| {
                stat_line(
                    r.label.clone(),
                    format_value(Some(r.value), r.kind, unit),
                    cx,
                )
            }),
        ));
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
pub fn push_capped<T>(series: &mut VecDeque<T>, value: T, cap: usize) {
    series.push_back(value);
    while series.len() > cap {
        series.pop_front();
    }
}

/// Quotes a CSV field when it contains a separator, quote or line break.
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

fn csv_header(labels: &[&str]) -> String {
    let fields: Vec<String> = labels.iter().map(|label| csv_field(label)).collect();
    format!("time_utc,{}\n", fields.join(","))
}

/// "2023-11-14T22:13:20Z,51.2,,40.0": missing values stay empty.
fn csv_row(time: u64, values: &[Option<f32>]) -> String {
    let mut row = utc_timestamp(time);
    for value in values {
        row.push(',');
        if let Some(v) = value {
            row.push_str(&format!("{v:.1}"));
        }
    }
    row.push('\n');
    row
}

/// Unix seconds as ISO 8601 UTC ("1970-01-01T00:00:00Z"), without a date crate.
fn utc_timestamp(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
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
    fn csv_lines() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(
            csv_row(0, &[Some(51.24), None, Some(40.0)]),
            "1970-01-01T00:00:00Z,51.2,,40.0\n"
        );
        assert_eq!(
            csv_header(&["CPU", "Fan \"1\", front"]),
            "time_utc,CPU,\"Fan \"\"1\"\", front\"\n"
        );
    }

    #[test]
    fn csv_aligns_late_sensors() {
        let mut dashboard = Dashboard::default();
        dashboard.times = VecDeque::from([0, 1]);
        dashboard
            .history
            .insert("a".into(), VecDeque::from([1.0, 2.0]));
        dashboard.history.insert("b".into(), VecDeque::from([3.0]));
        assert_eq!(
            dashboard.to_csv(),
            "time_utc,a,b\n1970-01-01T00:00:00Z,1.0,\n1970-01-01T00:00:01Z,2.0,3.0\n"
        );
    }

    #[test]
    fn threshold_steps() {
        assert_eq!(step_threshold(None, false, TEMP_STEP), None);
        assert_eq!(step_threshold(None, true, TEMP_STEP), Some(80.0));
        assert_eq!(step_threshold(Some(110.0), true, TEMP_STEP), Some(110.0));
        assert_eq!(step_threshold(Some(40.0), false, TEMP_STEP), None);
        assert_eq!(step_threshold(Some(300.0), false, RPM_STEP), Some(200.0));
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
