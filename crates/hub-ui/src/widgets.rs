//! Petits blocs de mise en page communs aux pages, et réglages du thème.
//!
//! Échelle d'espacement : 4 / 8 / 12 / 16 / 24 px (`gap_1`, `gap_2`, `gap_3`, `gap_4`, `gap_6`).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, Theme, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, Hsla, IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div, px, rgb,
};

/// Pastille pleine à texte blanc : `Tag::success()`/`danger()` (texte coloré sur fond coloré) sont peu lisibles.
pub fn solid_tag(color: u32) -> Tag {
    let fill = rgb(color).into();
    Tag::custom(fill, gpui_kit::white(), fill)
}

/// Vert et rouge des pastilles d'état.
pub const GREEN: u32 = 0x16a34a;
pub const RED: u32 = 0xdc2626;
/// Couleur d'accent unique (boutons principaux, interrupteurs, barres de consigne).
pub const ACCENT: u32 = 0x2563eb;
pub const ACCENT_HOVER: u32 = 0x3b82f6;
const ACCENT_ACTIVE: u32 = 0x1d4ed8;
/// Fond des cartes : un cran au-dessus du fond de fenêtre.
const CARD_BG: u32 = 0x141414;
/// Rayon commun des cartes.
pub const RADIUS: f32 = 10.0;

pub fn hex(color: u32) -> Hsla {
    rgb(color).into()
}

/// Thème sombre de gpui-kit + accent bleu.
/// Le texte du bouton « fermer » de la barre de titre au survol est `danger_foreground`
/// (rouge sur rouge dans le thème sombre par défaut) : on le passe en blanc.
pub fn apply_theme(cx: &mut App) {
    Theme::update(cx, |theme| {
        let white = gpui_kit::white();
        theme.danger_foreground = white;
        theme.primary = hex(ACCENT);
        theme.primary_hover = hex(ACCENT_HOVER);
        theme.primary_active = hex(ACCENT_ACTIVE);
        theme.primary_foreground = white;
        // Les boutons ont leurs propres couleurs, déduites de `primary` au chargement du thème.
        theme.button_primary = hex(ACCENT);
        theme.button_primary_hover = hex(ACCENT_HOVER);
        theme.button_primary_active = hex(ACCENT_ACTIVE);
        theme.button_primary_foreground = white;
        theme.ring = hex(ACCENT_HOVER);
        theme.slider_bar = hex(ACCENT);
        theme.slider_thumb = white;
    });
}

/// Cadre de carte : fond légèrement relevé, bordure discrète.
pub fn card(cx: &App) -> Div {
    v_flex()
        .gap_3()
        .p_4()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(cx.theme().border)
        .bg(hex(CARD_BG))
}

/// En-tête de carte : icône + titre.
pub fn card_header(icon: IconName, title: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .gap_2()
        .items_center()
        .text_sm()
        .font_semibold()
        .text_color(theme.foreground)
        .child(Icon::new(icon).small().text_color(theme.muted_foreground))
        .child(title.into())
}

/// Carte titrée (sans icône), pour les pages de réglages.
pub fn section(title: impl Into<SharedString>, cx: &App) -> Div {
    card(cx).child(div().text_sm().font_semibold().child(title.into()))
}

/// Ligne « libellé à gauche, contrôle à droite ».
pub fn field(label: impl Into<SharedString>, control: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .justify_between()
        .gap_4()
        .min_h(px(32.))
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label.into()),
        )
        .child(control)
}

/// Grande valeur suivie de son unité, plus discrète.
pub fn big_value(value: impl Into<SharedString>, unit: &str, color: Hsla, cx: &App) -> Div {
    h_flex()
        .items_baseline()
        .gap_1()
        .child(div().text_3xl().font_semibold().text_color(color).child(value.into()))
        .child(
            div()
                .text_base()
                .text_color(cx.theme().muted_foreground)
                .child(unit.to_owned()),
        )
}

/// Barre horizontale de consigne (0–100 %) ; vide si la consigne est inconnue (BIOS).
pub fn duty_bar(duty: Option<u8>, cx: &App) -> Div {
    let fill = f32::from(duty.unwrap_or(0).min(100)) / 100.0;
    div()
        .h(px(6.))
        .w_full()
        .rounded_full()
        .bg(cx.theme().border)
        .child(div().h_full().w(gpui_kit::relative(fill)).rounded_full().bg(hex(ACCENT)))
}

/// Texte secondaire.
pub fn muted(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Valeur numérique réglable par « − » / « + ».
pub fn stepper(
    id: &str,
    value: String,
    on_dec: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_inc: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let button = |suffix: &str, icon: IconName, handler: Handler| {
        Button::new(ElementId::Name(format!("{id}-{suffix}").into()))
            .small()
            .outline()
            .icon(icon)
            .on_click(handler)
    };
    h_flex()
        .gap_2()
        .child(button("dec", IconName::Minus, Box::new(on_dec)))
        .child(div().w(px(84.)).text_center().text_sm().child(value))
        .child(button("inc", IconName::Plus, Box::new(on_inc)))
        .into_any_element()
}

/// Bouton d'un groupe d'options exclusives (mode, page…).
pub fn choice(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Button {
    let button = Button::new(id).small().label(label);
    if selected { button.primary() } else { button.outline() }
}
