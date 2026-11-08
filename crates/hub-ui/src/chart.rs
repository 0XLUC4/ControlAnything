//! CPU and GPU temperature history chart (gpui-kit AreaChart).

use std::collections::VecDeque;

use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::{
    AnyElement, App, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, div, px, rgb,
};

use crate::prefs::TempUnit;
use crate::widgets::{SERIES_CPU as CPU_COLOR, SERIES_GPU as GPU_COLOR, loading};

/// Points drawn at most, whatever the time range (longer ranges are averaged).
const MAX_POINTS: usize = 120;
const CHART_HEIGHT: f32 = 240.0;

/// One instant of the chart; both values are aligned on the most recent point.
struct Point {
    label: SharedString,
    cpu: f32,
    gpu: f32,
}

/// Value at index `i` of a series right-aligned on `len` points;
/// before its first point, the series repeats its first value.
fn value_at(series: &VecDeque<f32>, len: usize, i: usize) -> f32 {
    let offset = len.saturating_sub(series.len());
    series.get(i.saturating_sub(offset)).copied().unwrap_or(0.0)
}

/// Bucket size so that `len` points fit in `max`.
fn bucket_size(len: usize, max: usize) -> usize {
    len.div_ceil(max.max(1)).max(1)
}

/// Averages consecutive buckets so that at most `max` points remain.
pub fn downsample(values: &[f32], max: usize) -> Vec<f32> {
    values
        .chunks(bucket_size(values.len(), max))
        .map(|chunk| chunk.iter().sum::<f32>() / chunk.len() as f32)
        .collect()
}

/// "-45s" under two minutes, "-12m" above.
fn ago_label(secs: u64) -> SharedString {
    if secs < 120 {
        format!("-{secs}s").into()
    } else {
        format!("-{}m", secs / 60).into()
    }
}

/// Chart body for the last `window_secs` seconds of `times` (unix seconds, one per snapshot).
pub fn temp_chart(
    cpu: Option<&VecDeque<f32>>,
    gpu: Option<&VecDeque<f32>>,
    times: &VecDeque<u64>,
    window_secs: u64,
    unit: TempUnit,
    cx: &App,
) -> AnyElement {
    let empty = VecDeque::new();
    let (cpu_series, gpu_series) = (cpu.unwrap_or(&empty), gpu.unwrap_or(&empty));
    let len = times.len();
    let latest = times.back().copied().unwrap_or(0);
    let start = times.partition_point(|&t| t + window_secs < latest);
    if len - start < 2 || (cpu_series.is_empty() && gpu_series.is_empty()) {
        return div()
            .h(px(CHART_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .child(loading("Waiting for data...", cx))
            .into_any_element();
    }
    let window = |series: &VecDeque<f32>| -> Vec<f32> {
        downsample(
            &(start..len)
                .map(|i| value_at(series, len, i))
                .collect::<Vec<_>>(),
            MAX_POINTS,
        )
    };
    let (cpu_points, gpu_points) = (window(cpu_series), window(gpu_series));
    let window_times: Vec<u64> = times.range(start..).copied().collect();
    let size = bucket_size(window_times.len(), MAX_POINTS);
    let points = window_times
        .chunks(size)
        .enumerate()
        .map(|(i, chunk)| Point {
            label: ago_label(latest - chunk.last().copied().unwrap_or(latest)),
            cpu: cpu_points[i],
            gpu: gpu_points[i],
        });
    let mut chart = AreaChart::new(points)
        .x(|p: &Point| p.label.clone())
        .y_domain(0.0, 100.0)
        .y_axis(true)
        .y_tick_format(move |v| unit.format(v as f32, 0))
        .x_tick_count(5)
        .appear(false);
    // A missing sensor (e.g. CPU temperature without admin rights) has no curve.
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
    div().h(px(CHART_HEIGHT)).child(chart).into_any_element()
}

/// "● CPU  ● GPU" legend.
pub fn legend(cx: &App) -> impl IntoElement {
    h_flex()
        .gap_3()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(div().text_color(color(CPU_COLOR)).child("● CPU"))
        .child(div().text_color(color(GPU_COLOR)).child("● GPU"))
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

    #[test]
    fn downsample_averages_buckets() {
        let values: Vec<f32> = (0..10).map(|v| v as f32).collect();
        assert_eq!(downsample(&values, 20), values);
        assert_eq!(downsample(&values, 5), [0.5, 2.5, 4.5, 6.5, 8.5]);
        // 10 points in 4: buckets of 3, the last one partial.
        assert_eq!(downsample(&values, 4), [1.0, 4.0, 7.0, 9.0]);
        assert!(downsample(&[], 4).is_empty());
        assert_eq!(downsample(&vec![1.0; 3600], MAX_POINTS).len(), MAX_POINTS);
    }

    #[test]
    fn ago_labels_switch_to_minutes() {
        assert_eq!(ago_label(45), "-45s");
        assert_eq!(ago_label(600), "-10m");
    }
}
