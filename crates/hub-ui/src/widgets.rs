//! Small layout blocks shared by pages, and theme settings.
//!
//! Spacing scale: 4 / 8 / 12 / 16 / 24 px (`gap_1`, `gap_2`, `gap_3`, `gap_4`, `gap_6`).

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

/// Solid badge with white text: `Tag::success()`/`danger()` (colored text on colored background) are hard to read.
pub fn solid_tag(color: u32) -> Tag {
    let fill = rgb(color).into();
    Tag::custom(fill, gpui_kit::white(), fill)
}

/// Green and red of the status badges.
pub const GREEN: u32 = 0x16a34a;
pub const RED: u32 = 0xdc2626;
/// Single accent color (primary buttons, switches, duty bars).
pub const ACCENT: u32 = 0x2563eb;
pub const ACCENT_HOVER: u32 = 0x3b82f6;
const ACCENT_ACTIVE: u32 = 0x1d4ed8;
/// Card background: one step above the window background.
const CARD_BG: u32 = 0x141414;
/// Shared card radius.
pub const RADIUS: f32 = 10.0;

pub fn hex(color: u32) -> Hsla {
    rgb(color).into()
}

/// gpui-kit dark theme + blue accent.
/// The title bar "close" button text on hover is `danger_foreground`
/// (red on red in the default dark theme): make it white.
pub fn apply_theme(cx: &mut App) {
    Theme::update(cx, |theme| {
        let white = gpui_kit::white();
        theme.danger_foreground = white;
        theme.primary = hex(ACCENT);
        theme.primary_hover = hex(ACCENT_HOVER);
        theme.primary_active = hex(ACCENT_ACTIVE);
        theme.primary_foreground = white;
        // Buttons have their own colors, derived from `primary` when the theme loads.
        theme.button_primary = hex(ACCENT);
        theme.button_primary_hover = hex(ACCENT_HOVER);
        theme.button_primary_active = hex(ACCENT_ACTIVE);
        theme.button_primary_foreground = white;
        theme.ring = hex(ACCENT_HOVER);
        theme.slider_bar = hex(ACCENT);
        theme.slider_thumb = white;
    });
}

/// Card frame: slightly raised background, subtle border.
pub fn card(cx: &App) -> Div {
    v_flex()
        .gap_3()
        .p_4()
        .rounded(px(RADIUS))
        .border_1()
        .border_color(cx.theme().border)
        .bg(hex(CARD_BG))
}

/// Card header: icon + title.
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

/// Titled card (no icon), for settings pages.
pub fn section(title: impl Into<SharedString>, cx: &App) -> Div {
    card(cx).child(div().text_sm().font_semibold().child(title.into()))
}

/// "Label on the left, control on the right" row.
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

/// Large value followed by its more subtle unit.
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

/// Horizontal duty bar (0-100 %); empty if the duty is unknown (BIOS).
pub fn duty_bar(duty: Option<u8>, cx: &App) -> Div {
    let fill = f32::from(duty.unwrap_or(0).min(100)) / 100.0;
    div()
        .h(px(6.))
        .w_full()
        .rounded_full()
        .bg(cx.theme().border)
        .child(div().h_full().w(gpui_kit::relative(fill)).rounded_full().bg(hex(ACCENT)))
}

/// Secondary text.
pub fn muted(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Numeric value adjustable with "-" / "+".
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

/// Button of an exclusive option group (mode, page...).
pub fn choice(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Button {
    let button = Button::new(id).small().label(label);
    if selected { button.primary() } else { button.outline() }
}
