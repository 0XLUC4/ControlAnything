//! Cartes du haut : processeur, carte graphique, mémoire.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{AnyElement, App, Hsla, IntoElement, ParentElement as _, Styled as _, div, rgb};
use hub_core::{Device, Reading, SensorKind};

use crate::dashboard::{find, find_kind, format_value};
use crate::prefs::TempUnit;
use crate::widgets::{self, big_value, card_header, duty_bar};

pub use crate::state::GPU_TEMP;
const GPU_LOAD: &str = "gpu0/load";
const GPU_POWER: &str = "gpu0/power";
const GPU_VRAM: &str = "gpu0/vram_used";
const GPU_FAN: &str = "gpu0/fan0";
const RAM_USED: &str = "ram/used";
const RAM_TOTAL: &str = "ram/total";
const RAM_LOAD: &str = "ram/load";

/// Seuils de couleur des températures (configurables en phase 2).
const TEMP_WARM: f32 = 60.0;
const TEMP_HOT: f32 = 80.0;
const GREEN: u32 = 0x22c55e;
const ORANGE: u32 = 0xf97316;
const RED: u32 = 0xef4444;

/// Couleur (hex RGB) d'une température : vert < 60, orange < 80, rouge au-delà.
pub fn temp_color(celsius: f32) -> u32 {
    if celsius < TEMP_WARM {
        GREEN
    } else if celsius < TEMP_HOT {
        ORANGE
    } else {
        RED
    }
}

/// Rangée des trois cartes, et les ids des mesures qu'elles affichent (exclues du tableau).
pub fn cards_row(readings: &[Reading], unit: TempUnit, cx: &App) -> (AnyElement, Vec<String>) {
    let cpu_temp = find_kind(readings, Device::Cpu, SensorKind::Temperature);
    let cpu_load = find_kind(readings, Device::Cpu, SensorKind::Load);
    let [
        gpu_temp,
        gpu_load,
        gpu_power,
        gpu_vram,
        gpu_fan,
        ram_used,
        ram_total,
        ram_load,
    ] = [
        GPU_TEMP, GPU_LOAD, GPU_POWER, GPU_VRAM, GPU_FAN, RAM_USED, RAM_TOTAL, RAM_LOAD,
    ]
    .map(|id| find(readings, id));
    let shown = [
        cpu_temp, cpu_load, gpu_temp, gpu_load, gpu_power, gpu_vram, gpu_fan, ram_used, ram_total,
        ram_load,
    ]
    .into_iter()
    .flatten()
    .map(|r| r.id.clone())
    .collect();

    let value = |r: Option<&Reading>| r.map(|r| r.value);
    let ram = match (value(ram_used), value(ram_total)) {
        (Some(used), Some(total)) => format!("{:.1} / {:.1} Gio", used / 1024.0, total / 1024.0),
        _ => "—".into(),
    };

    let row = h_flex()
        .gap_4()
        .items_stretch()
        .child(card(
            "Processeur",
            IconName::Cpu,
            value(cpu_temp),
            unit,
            vec![("Charge", format_value(value(cpu_load), SensorKind::Load, unit))],
            cx,
        ))
        .child(card(
            "Carte graphique",
            IconName::Gpu,
            value(gpu_temp),
            unit,
            vec![
                ("Charge", format_value(value(gpu_load), SensorKind::Load, unit)),
                (
                    "Puissance",
                    format_value(value(gpu_power), SensorKind::Power, unit),
                ),
                ("VRAM", format_value(value(gpu_vram), SensorKind::Memory, unit)),
                (
                    "Ventilateur",
                    format_value(value(gpu_fan), SensorKind::Load, unit),
                ),
            ],
            cx,
        ))
        .child(ram_card(ram, value(ram_load), cx))
        .into_any_element();
    (row, shown)
}

/// Carte avec une température en grand et des lignes de détail.
fn card(
    title: &str,
    icon: IconName,
    temp: Option<f32>,
    unit: TempUnit,
    lines: Vec<(&'static str, String)>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let color: Hsla = temp.map_or(theme.muted_foreground, |t| rgb(temp_color(t)).into());
    let big = temp.map_or("—".into(), |t| format!("{:.0}", unit.convert(t)));
    card_frame(title, icon, big_value(big, unit.symbol(), color, cx), cx).child(
        v_flex()
            .gap_1()
            .children(lines.into_iter().map(|(label, value)| detail(label, value, cx))),
    )
}

fn ram_card(used_total: String, load: Option<f32>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let big = load.map_or("—".into(), |l| format!("{l:.0}"));
    card_frame(
        "Mémoire",
        IconName::MemoryStick,
        big_value(big, "%", theme.foreground, cx),
        cx,
    )
    .child(duty_bar(load.map(|l| l.round().clamp(0.0, 100.0) as u8), cx))
    .child(detail("Utilisée", used_total, cx))
}

/// Ligne « libellé discret … valeur ».
fn detail(label: &'static str, value: String, cx: &App) -> impl IntoElement {
    h_flex()
        .justify_between()
        .text_sm()
        .child(div().text_color(cx.theme().muted_foreground).child(label))
        .child(value)
}

fn card_frame(title: &str, icon: IconName, big: gpui_kit::Div, cx: &App) -> gpui_kit::Div {
    widgets::card(cx)
        .flex_1()
        .min_w_0()
        .child(card_header(icon, title.to_owned(), cx))
        .child(big)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_thresholds() {
        assert_eq!(temp_color(45.0), GREEN);
        assert_eq!(temp_color(59.9), GREEN);
        assert_eq!(temp_color(60.0), ORANGE);
        assert_eq!(temp_color(79.9), ORANGE);
        assert_eq!(temp_color(80.0), RED);
    }
}
