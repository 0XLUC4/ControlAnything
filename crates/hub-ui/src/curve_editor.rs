//! Éditeur de courbe température → vitesse : dessin (canvas) et règles d'édition pures.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, BorderStyle, Bounds, Context, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, PathBuilder, Pixels, Point, Styled as _, Window, canvas, div,
    fill, point, px, quad, rgb, size,
};
use hub_core::config::CurvePoint;

use crate::prefs::TempUnit;

/// Bornes des axes : X en °C, Y en %.
pub const TEMP_MIN: f32 = 20.0;
pub const TEMP_MAX: f32 = 100.0;
/// Écart minimal entre deux points voisins (°C).
const MIN_GAP: f32 = 1.0;
/// Rayon de capture d'une poignée (px).
const HIT_RADIUS: f32 = 9.0;
const HANDLE_RADIUS: f32 = 5.0;
pub const WIDTH: f32 = 520.0;
pub const HEIGHT: f32 = 260.0;
const CURVE_COLOR: u32 = 0x38bdf8;
const MARKER_COLOR: u32 = 0xf97316;

/// (temp, duty) → position dans la zone de tracé (origine en haut à gauche).
pub fn to_px(temp: f32, duty: f32, width: f32, height: f32) -> (f32, f32) {
    let x = (temp - TEMP_MIN) / (TEMP_MAX - TEMP_MIN) * width;
    let y = (1.0 - duty / 100.0) * height;
    (x, y)
}

/// Position → (temp arrondie au degré, duty en %), bornées aux axes.
pub fn from_px(x: f32, y: f32, width: f32, height: f32) -> (f32, u8) {
    let temp = TEMP_MIN + (x / width).clamp(0.0, 1.0) * (TEMP_MAX - TEMP_MIN);
    let duty = ((1.0 - y / height).clamp(0.0, 1.0) * 100.0).round() as u8;
    (temp.round(), duty)
}

/// Index du point sous le curseur, s'il y en a un.
pub fn hit(points: &[CurvePoint], x: f32, y: f32, width: f32, height: f32) -> Option<usize> {
    points.iter().position(|p| {
        let (px, py) = to_px(p.temp, f32::from(p.duty), width, height);
        (px - x).hypot(py - y) <= HIT_RADIUS
    })
}

/// Déplace un point sans qu'il croise ses voisins : l'ordre (et donc l'index) est conservé.
pub fn move_point(points: &mut [CurvePoint], index: usize, temp: f32, duty: u8) {
    let low = index
        .checked_sub(1)
        .map_or(TEMP_MIN, |i| points[i].temp + MIN_GAP);
    let high = points
        .get(index + 1)
        .map_or(TEMP_MAX, |p| p.temp - MIN_GAP);
    if let Some(p) = points.get_mut(index) {
        p.temp = temp.max(low).min(high);
        p.duty = duty.min(100);
    }
}

/// Insère un point à sa place ; refusé s'il est trop proche d'un point existant.
pub fn add_point(points: &mut Vec<CurvePoint>, temp: f32, duty: u8) -> bool {
    if points.iter().any(|p| (p.temp - temp).abs() < MIN_GAP) {
        return false;
    }
    let at = points.partition_point(|p| p.temp < temp);
    points.insert(at, CurvePoint { temp, duty: duty.min(100) });
    true
}

/// Supprime un point en gardant au moins deux points.
pub fn remove_point(points: &mut Vec<CurvePoint>, index: usize) -> bool {
    if points.len() <= 2 || index >= points.len() {
        return false;
    }
    points.remove(index);
    true
}

/// État d'interaction de l'éditeur (le tracé lui-même vient du brouillon de config).
#[derive(Default)]
pub struct CurveEditor {
    /// Zone de tracé à l'écran, mise à jour à chaque frame (les évènements souris sont en coordonnées fenêtre).
    bounds: Rc<Cell<Bounds<Pixels>>>,
    pub dragging: Option<usize>,
}

impl CurveEditor {
    /// Position souris → coordonnées locales à la zone de tracé.
    pub fn local(&self, position: Point<Pixels>) -> (f32, f32) {
        let origin = self.bounds.get().origin;
        (
            f32::from(position.x - origin.x),
            f32::from(position.y - origin.y),
        )
    }

    /// Graphe avec axes, grille, courbe, poignées et repère « température actuelle / consigne ».
    /// Les gestionnaires souris sont fournis par la vue propriétaire.
    pub fn render<V: 'static>(
        &self,
        points: &[CurvePoint],
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
            move |b, (), window, _| paint(b, &points, marker, dragging, grid, window),
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

        let y_labels = div().relative().w(px(40.)).h(px(HEIGHT)).children(
            [100, 75, 50, 25, 0].map(|duty| {
                let (_, y) = to_px(TEMP_MIN, duty as f32, WIDTH, HEIGHT);
                div()
                    .absolute()
                    .top(px((y - 8.).min(HEIGHT - 16.)))
                    .right(px(8.))
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(label)
                    .child(format!("{duty} %"))
            }),
        );
        let x_labels = div().relative().ml(px(40.)).w(px(WIDTH)).h(px(22.)).children(
            [20, 40, 60, 80, 100].map(|temp| {
                let (x, _) = to_px(temp as f32, 0.0, WIDTH, HEIGHT);
                div()
                    .absolute()
                    .top(px(4.))
                    .left(px((x - 16.).clamp(0., WIDTH - 40.)))
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(label)
                    .child(unit.format(temp as f32, 0))
            }),
        );
        v_flex()
            .child(h_flex().child(y_labels).child(area))
            .child(x_labels)
            .into_any_element()
    }
}

/// Gestionnaires souris de la zone de tracé (appelés sur la vue `V`).
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
    marker: Option<(f32, f32)>,
    dragging: Option<usize>,
    grid: Hsla,
    window: &mut Window,
) {
    let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let at = |x: f32, y: f32| point(bounds.origin.x + px(x), bounds.origin.y + px(y));

    // Grille : tous les 10 °C et 10 %.
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

    let curve: Vec<(f32, f32)> = points
        .iter()
        .map(|p| to_px(p.temp, f32::from(p.duty), w, h))
        .collect();
    if let (Some(first), Some(last)) = (curve.first(), curve.last()) {
        // La courbe est bornée : plate avant le premier et après le dernier point.
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
        dot(window, at(x, y), 4.0, color(MARKER_COLOR), color(MARKER_COLOR));
    }

    for (i, &(x, y)) in curve.iter().enumerate() {
        let radius = if dragging == Some(i) { HANDLE_RADIUS + 2.0 } else { HANDLE_RADIUS };
        dot(window, at(x, y), radius, gpui_kit::white(), color(CURVE_COLOR));
    }
}

/// Disque centré sur `center`, bordé de `border`.
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

/// Libellé court d'un mode (pour l'aide sous le graphe).
pub fn help_text(app: &App) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(app.theme().muted_foreground)
        .child("Glisser : déplacer un point · Double-clic : ajouter · Clic droit : supprimer")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(list: &[(f32, u8)]) -> Vec<CurvePoint> {
        list.iter().map(|&(temp, duty)| CurvePoint { temp, duty }).collect()
    }

    #[test]
    fn pixel_mapping_roundtrip() {
        assert_eq!(to_px(20.0, 100.0, 400.0, 200.0), (0.0, 0.0));
        assert_eq!(to_px(100.0, 0.0, 400.0, 200.0), (400.0, 200.0));
        assert_eq!(from_px(200.0, 100.0, 400.0, 200.0), (60.0, 50));
        // Hors zone : borné aux axes.
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
        assert_eq!(points[1], CurvePoint { temp: 69.0, duty: 100 });
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
        assert_eq!(points.iter().map(|p| p.temp).collect::<Vec<_>>(), [30.0, 50.0, 70.0]);
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
