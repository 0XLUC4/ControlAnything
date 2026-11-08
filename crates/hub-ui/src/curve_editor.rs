//! Temperature → speed curve editor: drawing (canvas) and pure editing rules.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, BorderStyle, Bounds, Context, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, PathBuilder, Pixels, Point, Styled as _, Window, canvas, div,
    fill, point, px, quad, rgb, size,
};
use hub_core::config::{CurvePoint, DutyRange};

use crate::prefs::TempUnit;

/// Axis bounds: X in °C, Y in %.
pub const TEMP_MIN: f32 = 20.0;
pub const TEMP_MAX: f32 = 100.0;
/// Minimum gap between two neighbouring points (°C).
const MIN_GAP: f32 = 1.0;
/// Handle hit radius (px).
const HIT_RADIUS: f32 = 9.0;
const HANDLE_RADIUS: f32 = 5.0;
pub const WIDTH: f32 = 520.0;
pub const HEIGHT: f32 = 260.0;
use crate::widgets::{MARKER as MARKER_COLOR, RED as AVOID_COLOR, SERIES_CPU as CURVE_COLOR};

/// (temp, duty) → position in the plot area (origin at top left).
pub fn to_px(temp: f32, duty: f32, width: f32, height: f32) -> (f32, f32) {
    let x = (temp - TEMP_MIN) / (TEMP_MAX - TEMP_MIN) * width;
    let y = (1.0 - duty / 100.0) * height;
    (x, y)
}

/// Position → (temp rounded to the degree, duty in %), clamped to the axes.
pub fn from_px(x: f32, y: f32, width: f32, height: f32) -> (f32, u8) {
    let temp = TEMP_MIN + (x / width).clamp(0.0, 1.0) * (TEMP_MAX - TEMP_MIN);
    let duty = ((1.0 - y / height).clamp(0.0, 1.0) * 100.0).round() as u8;
    (temp.round(), duty)
}

/// Index of the point under the cursor, if any.
pub fn hit(points: &[CurvePoint], x: f32, y: f32, width: f32, height: f32) -> Option<usize> {
    points.iter().position(|p| {
        let (px, py) = to_px(p.temp, f32::from(p.duty), width, height);
        (px - x).hypot(py - y) <= HIT_RADIUS
    })
}

/// Moves a point without crossing its neighbours: the order (and so the index) is kept.
pub fn move_point(points: &mut [CurvePoint], index: usize, temp: f32, duty: u8) {
    let low = index
        .checked_sub(1)
        .map_or(TEMP_MIN, |i| points[i].temp + MIN_GAP);
    let high = points.get(index + 1).map_or(TEMP_MAX, |p| p.temp - MIN_GAP);
    if let Some(p) = points.get_mut(index) {
        p.temp = temp.max(low).min(high);
        p.duty = duty.min(100);
    }
}

/// Inserts a point in order; rejected if too close to an existing point.
pub fn add_point(points: &mut Vec<CurvePoint>, temp: f32, duty: u8) -> bool {
    if points.iter().any(|p| (p.temp - temp).abs() < MIN_GAP) {
        return false;
    }
    let at = points.partition_point(|p| p.temp < temp);
    points.insert(
        at,
        CurvePoint {
            temp,
            duty: duty.min(100),
        },
    );
    true
}

/// Removes a point while keeping at least two points.
pub fn remove_point(points: &mut Vec<CurvePoint>, index: usize) -> bool {
    if points.len() <= 2 || index >= points.len() {
        return false;
    }
    points.remove(index);
    true
}

/// Editor interaction state (the curve itself comes from the config draft).
#[derive(Default)]
pub struct CurveEditor {
    /// On-screen plot area, updated every frame (mouse events are in window coordinates).
    bounds: Rc<Cell<Bounds<Pixels>>>,
    pub dragging: Option<usize>,
}

impl CurveEditor {
    /// Mouse position → coordinates local to the plot area.
    pub fn local(&self, position: Point<Pixels>) -> (f32, f32) {
        let origin = self.bounds.get().origin;
        (
            f32::from(position.x - origin.x),
            f32::from(position.y - origin.y),
        )
    }

    /// Chart with axes, grid, avoided speed band, curve, handles and a "current temperature / target"
    /// marker. Mouse handlers are provided by the owning view.
    pub fn render<V: 'static>(
        &self,
        points: &[CurvePoint],
        avoid: Option<DutyRange>,
        marker: Option<(f32, f32)>,
        handlers: CurveHandlers<V>,
        unit: TempUnit,
        cx: &mut Context<V>,
    ) -> AnyElement {
        let theme = cx.theme();
        let grid = theme.border;
        let label = theme.muted_foreground;
        let bounds = self.bounds.clone();
        let points = points.to_vec();
        let dragging = self.dragging;
        let plot = canvas(
            move |b, _, _| bounds.set(b),
            move |b, (), window, _| paint(b, &points, avoid, marker, dragging, grid, window),
        )
        .size_full();

        let area = div()
            .id("curve-area")
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .cursor_crosshair()
            .child(plot)
            .on_mouse_down(MouseButton::Left, cx.listener(handlers.down))
            .on_mouse_down(MouseButton::Right, cx.listener(handlers.right))
            .on_mouse_move(cx.listener(handlers.moved))
            .on_mouse_up(MouseButton::Left, cx.listener(handlers.up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(handlers.up_out));

        let y_labels =
            div()
                .relative()
                .w(px(40.))
                .h(px(HEIGHT))
                .children([100, 75, 50, 25, 0].map(|duty| {
                    let (_, y) = to_px(TEMP_MIN, duty as f32, WIDTH, HEIGHT);
                    div()
                        .absolute()
                        .top(px((y - 8.).min(HEIGHT - 16.)))
                        .right(px(8.))
                        .whitespace_nowrap()
                        .text_xs()
                        .text_color(label)
                        .child(format!("{duty} %"))
                }));
        let x_labels = div()
            .relative()
            .ml(px(40.))
            .w(px(WIDTH))
            .h(px(22.))
            .children([20, 40, 60, 80, 100].map(|temp| {
                let (x, _) = to_px(temp as f32, 0.0, WIDTH, HEIGHT);
                div()
                    .absolute()
                    .top(px(4.))
                    .left(px((x - 16.).clamp(0., WIDTH - 40.)))
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(label)
                    .child(unit.format(temp as f32, 0))
            }));
        v_flex()
            .child(h_flex().child(y_labels).child(area))
            .child(x_labels)
            .into_any_element()
    }
}

/// Mouse handlers of the plot area (called on view `V`).
pub struct CurveHandlers<V> {
    pub down: fn(&mut V, &gpui_kit::MouseDownEvent, &mut Window, &mut Context<V>),
    pub right: fn(&mut V, &gpui_kit::MouseDownEvent, &mut Window, &mut Context<V>),
    pub moved: fn(&mut V, &gpui_kit::MouseMoveEvent, &mut Window, &mut Context<V>),
    pub up: fn(&mut V, &gpui_kit::MouseUpEvent, &mut Window, &mut Context<V>),
    pub up_out: fn(&mut V, &gpui_kit::MouseUpEvent, &mut Window, &mut Context<V>),
}

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn paint(
    bounds: Bounds<Pixels>,
    points: &[CurvePoint],
    avoid: Option<DutyRange>,
    marker: Option<(f32, f32)>,
    dragging: Option<usize>,
    grid: Hsla,
    window: &mut Window,
) {
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let at = |x: f32, y: f32| point(bounds.origin.x + px(x), bounds.origin.y + px(y));

    // Grid: every 10 °C and 10 %.
    for i in 0..=8 {
        let (x, _) = to_px(TEMP_MIN + i as f32 * 10.0, 0.0, w, h);
        window.paint_quad(fill(
            Bounds::new(at(x.min(w - 1.0), 0.0), size(px(1.), px(h))),
            grid,
        ));
    }
    for i in 0..=10 {
        let (_, y) = to_px(TEMP_MIN, i as f32 * 10.0, w, h);
        window.paint_quad(fill(
            Bounds::new(at(0.0, y.min(h - 1.0)), size(px(w), px(1.))),
            grid,
        ));
    }

    if let Some(band) = avoid {
        let (_, top) = to_px(TEMP_MIN, f32::from(band.max), w, h);
        let (_, bottom) = to_px(TEMP_MIN, f32::from(band.min), w, h);
        window.paint_quad(fill(
            Bounds::new(at(0.0, top), size(px(w), px(bottom - top))),
            color(AVOID_COLOR).opacity(0.12),
        ));
    }

    let curve: Vec<(f32, f32)> = points
        .iter()
        .map(|p| to_px(p.temp, f32::from(p.duty), w, h))
        .collect();
    if let (Some(first), Some(last)) = (curve.first(), curve.last()) {
        // The curve is clamped: flat before the first and after the last point.
        let mut line = PathBuilder::stroke(px(2.));
        line.move_to(at(0.0, first.1));
        for &(x, y) in &curve {
            line.line_to(at(x, y));
        }
        line.line_to(at(w, last.1));
        if let Ok(path) = line.build() {
            window.paint_path(path, color(CURVE_COLOR));
        }
    }

    if let Some((temp, duty)) = marker {
        let (x, y) = to_px(temp.clamp(TEMP_MIN, TEMP_MAX), duty, w, h);
        window.paint_quad(fill(
            Bounds::new(at(x, 0.0), size(px(1.), px(h))),
            color(MARKER_COLOR).opacity(0.6),
        ));
        dot(
            window,
            at(x, y),
            4.0,
            color(MARKER_COLOR),
            color(MARKER_COLOR),
        );
    }

    for (i, &(x, y)) in curve.iter().enumerate() {
        let radius = if dragging == Some(i) {
            HANDLE_RADIUS + 2.0
        } else {
            HANDLE_RADIUS
        };
        dot(
            window,
            at(x, y),
            radius,
            gpui_kit::white(),
            color(CURVE_COLOR),
        );
    }
}

/// Disc centred on `center`, outlined with `border`.
fn dot(window: &mut Window, center: Point<Pixels>, radius: f32, background: Hsla, border: Hsla) {
    let r = px(radius);
    window.paint_quad(quad(
        Bounds::new(point(center.x - r, center.y - r), size(r * 2., r * 2.)),
        r,
        background,
        px(2.),
        border,
        BorderStyle::default(),
    ));
}

/// Short help text shown under the chart.
pub fn help_text(app: &App) -> impl IntoElement {
    h_flex()
        .gap_1()
        .text_xs()
        .text_color(app.theme().muted_foreground)
        .child(Icon::new(IconName::Info).xsmall())
        .child("Drag to move a point, double-click to add, right-click to remove")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(list: &[(f32, u8)]) -> Vec<CurvePoint> {
        list.iter()
            .map(|&(temp, duty)| CurvePoint { temp, duty })
            .collect()
    }

    #[test]
    fn pixel_mapping_roundtrip() {
        assert_eq!(to_px(20.0, 100.0, 400.0, 200.0), (0.0, 0.0));
        assert_eq!(to_px(100.0, 0.0, 400.0, 200.0), (400.0, 200.0));
        assert_eq!(from_px(200.0, 100.0, 400.0, 200.0), (60.0, 50));
        // Outside the area: clamped to the axes.
        assert_eq!(from_px(-50.0, 500.0, 400.0, 200.0), (20.0, 0));
        assert_eq!(from_px(900.0, -5.0, 400.0, 200.0), (100.0, 100));
    }

    #[test]
    fn hit_finds_nearby_handle() {
        let points = pts(&[(20.0, 100), (60.0, 50)]);
        assert_eq!(hit(&points, 3.0, 2.0, 400.0, 200.0), Some(0));
        assert_eq!(hit(&points, 200.0, 105.0, 400.0, 200.0), Some(1));
        assert_eq!(hit(&points, 300.0, 100.0, 400.0, 200.0), None);
    }

    #[test]
    fn dragging_cannot_cross_neighbours() {
        let mut points = pts(&[(30.0, 20), (50.0, 40), (70.0, 80)]);
        move_point(&mut points, 1, 90.0, 150);
        assert_eq!(
            points[1],
            CurvePoint {
                temp: 69.0,
                duty: 100
            }
        );
        move_point(&mut points, 1, 10.0, 0);
        assert_eq!(points[1].temp, 31.0);
        move_point(&mut points, 0, 5.0, 10);
        assert_eq!(points[0].temp, TEMP_MIN);
        move_point(&mut points, 2, 120.0, 10);
        assert_eq!(points[2].temp, TEMP_MAX);
    }

    #[test]
    fn add_keeps_sorted_and_rejects_duplicates() {
        let mut points = pts(&[(30.0, 20), (70.0, 80)]);
        assert!(add_point(&mut points, 50.0, 40));
        assert_eq!(
            points.iter().map(|p| p.temp).collect::<Vec<_>>(),
            [30.0, 50.0, 70.0]
        );
        assert!(!add_point(&mut points, 50.5, 40));
        assert!(add_point(&mut points, 90.0, 255));
        assert_eq!(points[3].duty, 100);
    }

    #[test]
    fn remove_keeps_two_points() {
        let mut points = pts(&[(30.0, 20), (50.0, 40), (70.0, 80)]);
        assert!(remove_point(&mut points, 1));
        assert!(!remove_point(&mut points, 0));
        assert!(!remove_point(&mut points, 5));
        assert_eq!(points.len(), 2);
    }
}
