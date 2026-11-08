//! Graphe temps réel des températures CPU et GPU (AreaChart de gpui-kit).

use std::collections::VecDeque;

use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::{
    AnyElement, App, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, div, px, rgb,
};

use crate::prefs::TempUnit;

const CPU_COLOR: u32 = 0x38bdf8;
const GPU_COLOR: u32 = 0xa78bfa;

/// Un instant du graphe ; les deux valeurs sont alignées sur le point le plus récent.
struct Point {
    label: SharedString,
    cpu: f32,
    gpu: f32,
}

/// Valeur au rang `i` d'une série alignée à droite sur `len` points ;
/// avant son premier point, la série reprend sa première valeur.
fn value_at(series: &VecDeque<f32>, len: usize, i: usize) -> f32 {
    let offset = len.saturating_sub(series.len());
    series.get(i.saturating_sub(offset)).copied().unwrap_or(0.0)
}

pub fn temp_chart(
    cpu: Option<&VecDeque<f32>>,
    gpu: Option<&VecDeque<f32>>,
    unit: TempUnit,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let empty = VecDeque::new();
    let (cpu_series, gpu_series) = (cpu.unwrap_or(&empty), gpu.unwrap_or(&empty));
    let len = cpu_series.len().max(gpu_series.len());

    let body: AnyElement = if len < 2 {
        div()
            .h(px(240.))
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.muted_foreground)
            .child("En attente de données…")
            .into_any_element()
    } else {
        let points = (0..len).map(|i| Point {
            label: format!("-{}s", len - 1 - i).into(),
            cpu: value_at(cpu_series, len, i),
            gpu: value_at(gpu_series, len, i),
        });
        let mut chart = AreaChart::new(points)
            .x(|p: &Point| p.label.clone())
            .y_domain(0.0, 100.0)
            .y_axis(true)
            .y_tick_format(move |v| unit.format(v as f32, 0))
            .x_tick_count(5)
            .appear(false);
        // Un capteur absent (ex. température CPU sans droits admin) n'a pas de courbe.
        if !cpu_series.is_empty() {
            chart = chart
                .y(|p: &Point| p.cpu)
                .linear()
                .stroke(color(CPU_COLOR))
                .fill(color(CPU_COLOR).opacity(0.1))
                .name("CPU");
        }
        if !gpu_series.is_empty() {
            chart = chart
                .y(|p: &Point| p.gpu)
                .linear()
                .stroke(color(GPU_COLOR))
                .fill(color(GPU_COLOR).opacity(0.1))
                .name("GPU");
        }
        div().h(px(240.)).child(chart).into_any_element()
    };

    crate::widgets::card(cx)
        .child(
            h_flex()
                .gap_4()
                .text_sm()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("Températures (2 min)"),
                )
                .child(div().text_color(color(CPU_COLOR)).child("● CPU"))
                .child(div().text_color(color(GPU_COLOR)).child("● GPU")),
        )
        .child(body)
        .into_any_element()
}

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_series_is_right_aligned() {
        let series = VecDeque::from([10.0, 20.0]);
        let values: Vec<f32> = (0..4).map(|i| value_at(&series, 4, i)).collect();
        assert_eq!(values, [10.0, 10.0, 10.0, 20.0]);
    }
}
