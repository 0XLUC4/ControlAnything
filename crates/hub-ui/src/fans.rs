//! "Fans" page: channel list and settings editor for the selected channel.

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableListItem;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, Entity, InteractiveElement as _, IntoElement,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use hub_core::config::{Config, CurvePoint, DutyRange, FanMode, FanSettings, Preset, TempSource};
use hub_core::control::{evaluate_curve, resolve_source};
use hub_core::fan::FanChannel;

use crate::app::HubApp;
use crate::curve_editor::{self, CurveEditor, CurveHandlers, HEIGHT, WIDTH};
use crate::state::{
    FanGroup, channel_rpm, display_name, fan_group, is_pump, mode_label, reset_active_profile,
    reset_fan, settings_for, settings_mut, temp_sources,
};
use crate::widgets::{
    MARKER, card, duty_bar, field, format_rpm, group_label, hex, loading, muted, page_header,
    section, segmented, selectable_row, stepper,
};

/// How long "Confirm reset" stays offered.
const RESET_CONFIRM: Duration = Duration::from_secs(4);
/// Minimum duty enforced on pumps by the service.
const PUMP_MIN_DUTY: u8 = 60;
/// Presets offered in the page header, applied to every fan at once.
const PRESETS: [(&str, Preset); 3] = [
    ("Silent", Preset::Silent),
    ("Balanced", Preset::Balanced),
    ("Performance", Preset::Performance),
];
/// Mode picker entries: (label, key for `set_mode`, description).
const MODES: [(&str, &str, &str); 3] = [
    ("Auto", "auto", "The BIOS / firmware controls this fan."),
    (
        "Fixed",
        "fixed",
        "Constant speed, whatever the temperature.",
    ),
    (
        "Curve",
        "curve",
        "Speed follows a temperature, as drawn below.",
    ),
];
/// Avoided band offered when the option is switched on.
const DEFAULT_AVOID: DutyRange = DutyRange { min: 40, max: 55 };

/// Source selector item: displayed label + temperature source.
#[derive(Clone, PartialEq)]
pub struct SourceItem {
    title: SharedString,
    source: TempSource,
}

impl SearchableListItem for SourceItem {
    type Value = TempSource;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &TempSource {
        &self.source
    }
}

/// Page state independent of the window.
#[derive(Default)]
pub struct FansPage {
    pub selected: Option<String>,
    /// Items currently loaded in `FanControls::source_select`.
    pub source_items: Vec<SourceItem>,
    pub curve: CurveEditor,
    /// "Not connected" list expanded.
    pub show_unconnected: bool,
    /// "Advanced" section expanded.
    pub show_advanced: bool,
}

/// Stateful page controls (tied to the window); their value is copied from the draft on each render.
#[derive(Clone)]
pub struct FanControls {
    name_input: Entity<InputState>,
    fixed_slider: Entity<SliderState>,
    min_slider: Entity<SliderState>,
    source_select: Entity<SelectState<Vec<SourceItem>>>,
}

fn percent_slider(cx: &mut Context<HubApp>) -> Entity<SliderState> {
    cx.new(|_| SliderState::new().min(0.).max(100.).step(1.))
}

impl FanControls {
    pub fn new(window: &mut Window, cx: &mut Context<HubApp>) -> (Self, Vec<Subscription>) {
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Fan name"));
        let fixed_slider = percent_slider(cx);
        let min_slider = percent_slider(cx);
        let source_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));

        let subscriptions = vec![
            cx.subscribe_in(
                &name_input,
                window,
                |app: &mut HubApp, input, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let name = input.read(cx).value().trim().to_owned();
                        app.rename_selected(name, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &fixed_slider,
                window,
                |app: &mut HubApp, _, event, _, cx| {
                    let SliderEvent::Change(value) = event else {
                        return;
                    };
                    let duty = value.start().round() as u8;
                    app.edit_fan(cx, |s| {
                        if let FanMode::Fixed { duty: d } = &mut s.mode {
                            *d = duty;
                        }
                    });
                },
            ),
            cx.subscribe_in(&min_slider, window, |app: &mut HubApp, _, event, _, cx| {
                if let SliderEvent::Change(value) = event {
                    let duty = value.start().round() as u8;
                    app.edit_fan(cx, |s| s.min_duty = duty);
                }
            }),
            cx.subscribe_in(
                &source_select,
                window,
                |app: &mut HubApp, _, event: &SelectEvent<Vec<SourceItem>>, _, cx| {
                    if let SelectEvent::Confirm(Some(source)) = event {
                        let source = source.clone();
                        app.edit_fan(cx, |s| {
                            if let FanMode::Curve {
                                source: current, ..
                            } = &mut s.mode
                            {
                                *current = source;
                            }
                        });
                    }
                },
            ),
        ];
        let controls = Self {
            name_input,
            fixed_slider,
            min_slider,
            source_select,
        };
        (controls, subscriptions)
    }
}

/// Applies `preset` to every channel except pumps (they keep their safe settings).
/// Channels not in Curve mode switch to a curve on `source`; existing curves keep theirs.
/// Returns `true` if the config changed.
pub fn apply_preset(
    config: &mut Config,
    channels: &[FanChannel],
    preset: Preset,
    source: &TempSource,
) -> bool {
    let before = config.clone();
    for channel in channels.iter().filter(|c| !is_pump(c)) {
        let Some(settings) = settings_mut(config, &channel.id) else {
            continue;
        };
        if !matches!(settings.mode, FanMode::Curve { .. }) {
            settings.mode = FanMode::Curve {
                source: source.clone(),
                points: Vec::new(),
            };
        }
        preset.apply(settings);
    }
    *config != before
}

/// Preset matching every non-pump channel, if any.
fn current_preset(config: &Config, channels: &[FanChannel]) -> Option<usize> {
    let mut fans = channels.iter().filter(|c| !is_pump(c)).peekable();
    fans.peek()?;
    let all: Vec<FanSettings> = fans.map(|c| settings_for(config, &c.id)).collect();
    PRESETS.iter().position(|(_, preset)| {
        all.iter()
            .all(|s| matches!(s.mode, FanMode::Curve { .. }) && preset.matches(s))
    })
}

impl HubApp {
    /// Edits the selected channel settings in the draft.
    fn edit_fan(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut FanSettings)) {
        let (Some(channel), Some(draft)) =
            (self.fans.selected.as_deref(), self.state.draft.as_mut())
        else {
            return;
        };
        if let Some(settings) = settings_mut(draft, channel) {
            let before = settings.clone();
            edit(settings);
            if *settings != before {
                cx.notify();
            }
        }
    }

    /// Puts the selected channel back to Auto (BIOS) in the draft.
    fn reset_selected_fan(&mut self, cx: &mut Context<Self>) {
        let (Some(channel), Some(draft)) =
            (self.fans.selected.as_deref(), self.state.draft.as_mut())
        else {
            return;
        };
        if reset_fan(draft, channel) {
            self.fans.curve.dragging = None;
            cx.notify();
        }
    }

    /// "Reset profile": first click → confirmation asked for a few seconds,
    /// second click → every channel of the active profile goes back to Auto (in the draft,
    /// to be confirmed with "Apply" like any change).
    pub fn reset_profile_button(&self, id: &'static str, cx: &mut Context<Self>) -> Button {
        let confirming = self
            .confirm_reset_until
            .is_some_and(|until| Instant::now() < until);
        let empty = self
            .state
            .draft
            .as_ref()
            .and_then(|d| d.active())
            .is_none_or(|p| p.fans.is_empty());
        let button = Button::new(id).small().icon(IconName::RotateCcw);
        let button = if confirming {
            button.danger().label("Confirm reset")
        } else {
            button.outline().label("Reset profile")
        };
        button
            .tooltip("Every fan of this profile goes back to Auto (BIOS)")
            .disabled(self.is_pending() || (empty && !confirming))
            .on_click(cx.listener(|app, _, _, cx| {
                let now = Instant::now();
                if app
                    .confirm_reset_until
                    .take()
                    .is_some_and(|until| now < until)
                {
                    if let Some(draft) = app.state.draft.as_mut() {
                        reset_active_profile(draft);
                    }
                    app.fans.curve.dragging = None;
                } else {
                    app.confirm_reset_until = Some(now + RESET_CONFIRM);
                }
                cx.notify();
            }))
    }

    fn selected_settings(&self) -> Option<FanSettings> {
        let channel = self.fans.selected.as_deref()?;
        self.state.draft.as_ref().map(|d| settings_for(d, channel))
    }

    fn rename_selected(&mut self, name: String, cx: &mut Context<Self>) {
        let (Some(channel), Some(draft)) = (self.fans.selected.clone(), self.state.draft.as_mut())
        else {
            return;
        };
        let changed = if name.is_empty() {
            draft.fan_labels.remove(&channel).is_some()
        } else {
            draft.fan_labels.insert(channel, name.clone()) != Some(name)
        };
        if changed {
            cx.notify();
        }
    }

    /// First offered temperature source (CPU package if nothing is read yet).
    fn default_source(&self) -> TempSource {
        temp_sources(self.state.readings())
            .into_iter()
            .next()
            .map_or(
                TempSource::Sensor {
                    id: "cpu/package_temp".into(),
                },
                |(_, source)| source,
            )
    }

    /// Channels with a fan plugged in (presets ignore empty headers).
    fn connected_channels(&self) -> Vec<FanChannel> {
        self.state
            .channels
            .iter()
            .filter(|c| self.state.is_connected(c))
            .cloned()
            .collect()
    }

    /// Header preset buttons: the preset goes to every fan of the active profile.
    fn apply_preset_all(&mut self, preset: Preset, cx: &mut Context<Self>) {
        let channels = self.connected_channels();
        let source = self.default_source();
        let Some(draft) = self.state.draft.as_mut() else {
            return;
        };
        if apply_preset(draft, &channels, preset, &source) {
            self.fans.curve.dragging = None;
            cx.notify();
        }
    }

    /// Switches mode starting from useful values (current duty, Balanced preset).
    fn set_mode(&mut self, mode: &'static str, cx: &mut Context<Self>) {
        let applied = self
            .fans
            .selected
            .as_deref()
            .and_then(|c| self.state.applied_duty(c))
            .flatten();
        let default_source = self.default_source();
        self.edit_fan(cx, |s| {
            let same = matches!(
                (&s.mode, mode),
                (FanMode::Auto, "auto")
                    | (FanMode::Fixed { .. }, "fixed")
                    | (FanMode::Curve { .. }, "curve")
            );
            if same {
                return;
            }
            s.mode = match mode {
                "fixed" => FanMode::Fixed {
                    duty: applied.unwrap_or(50),
                },
                "curve" => FanMode::Curve {
                    source: default_source,
                    points: Preset::Balanced.points(),
                },
                _ => FanMode::Auto,
            };
        });
    }

    fn edit_points(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Vec<CurvePoint>)) {
        self.edit_fan(cx, |s| {
            if let FanMode::Curve { points, .. } = &mut s.mode {
                edit(points);
            }
        });
    }

    fn curve_points(&self) -> Vec<CurvePoint> {
        match self.selected_settings().map(|s| s.mode) {
            Some(FanMode::Curve { points, .. }) => points,
            _ => Vec::new(),
        }
    }

    fn curve_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (x, y) = self.fans.curve.local(event.position);
        let points = self.curve_points();
        match curve_editor::hit(&points, x, y, WIDTH, HEIGHT) {
            Some(index) => self.fans.curve.dragging = Some(index),
            None if event.click_count == 2 => {
                let (temp, duty) = curve_editor::from_px(x, y, WIDTH, HEIGHT);
                self.edit_points(cx, |p| {
                    curve_editor::add_point(p, temp, duty);
                });
            }
            None => {}
        }
        cx.notify();
    }

    fn curve_right(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (x, y) = self.fans.curve.local(event.position);
        if let Some(index) = curve_editor::hit(&self.curve_points(), x, y, WIDTH, HEIGHT) {
            self.edit_points(cx, |p| {
                curve_editor::remove_point(p, index);
            });
        }
    }

    fn curve_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.fans.curve.dragging else {
            return;
        };
        if event.pressed_button.is_none() {
            self.fans.curve.dragging = None;
            cx.notify();
            return;
        }
        let (x, y) = self.fans.curve.local(event.position);
        let (temp, duty) = curve_editor::from_px(x, y, WIDTH, HEIGHT);
        self.edit_points(cx, |p| curve_editor::move_point(p, index, temp, duty));
    }

    fn curve_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.fans.curve.dragging.take().is_some() {
            cx.notify();
        }
    }

    /// Copies the draft into the stateful controls (no effect if already up to date).
    fn sync_fan_controls(
        &mut self,
        controls: &FanControls,
        channel: &FanChannel,
        settings: &FanSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = self
            .state
            .draft
            .as_ref()
            .and_then(|d| d.fan_labels.get(&channel.id))
            .cloned()
            .unwrap_or_default();
        controls.name_input.update(cx, |input, cx| {
            if input.value().trim() != label {
                input.set_value(label, window, cx);
            }
        });
        let sync_slider = |slider: &Entity<SliderState>,
                           value: u8,
                           window: &mut Window,
                           cx: &mut Context<Self>| {
            slider.update(cx, |slider, cx| {
                if slider.value().start().round() as u8 != value {
                    slider.set_value(f32::from(value), window, cx);
                }
            });
        };
        if let FanMode::Fixed { duty } = settings.mode {
            sync_slider(&controls.fixed_slider, duty, window, cx);
        }
        sync_slider(&controls.min_slider, settings.min_duty, window, cx);

        if let FanMode::Curve { source, .. } = &settings.mode {
            let items: Vec<SourceItem> = temp_sources(self.state.readings())
                .into_iter()
                .map(|(title, source)| SourceItem {
                    title: title.into(),
                    source,
                })
                .collect();
            let items_changed = items != self.fans.source_items;
            if items_changed {
                self.fans.source_items.clone_from(&items);
            }
            controls.source_select.update(cx, |select, cx| {
                if items_changed {
                    select.set_items(items, window, cx);
                }
                if items_changed || select.selected_value() != Some(source) {
                    select.set_selected_value(source, window, cx);
                }
            });
        }
    }

    pub fn render_fans(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.state.draft.is_none() {
            return loading("Waiting for the service configuration...", cx);
        }
        let profile = self
            .state
            .draft
            .as_ref()
            .map(|d| d.active_profile.clone())
            .unwrap_or_default();
        let header = page_header(IconName::Fan, "Fans", format!("Profile: {profile}"), cx).child(
            h_flex()
                .gap_3()
                .child(self.preset_picker(cx))
                .child(self.reset_profile_button("reset-profile-fans", cx)),
        );
        let page = v_flex().gap_4().child(header).children(
            self.automations.active.as_ref().map(|profile| {
                Alert::info(
                    "fans-automation-active",
                    format!("Automation active: {profile}, manual changes apply when it ends"),
                )
            }),
        );
        if self.state.channels.is_empty() {
            return page
                .child(Alert::warning(
                    "no-fans",
                    "No controllable fan detected: run the service as administrator.",
                ))
                .into_any_element();
        }
        let selected = self
            .fans
            .selected
            .as_deref()
            .and_then(|id| self.state.channels.iter().find(|c| c.id == id))
            .cloned();
        let editor = selected.map(|channel| self.fan_editor(&channel, window, cx));
        let board_missing = !self
            .state
            .channels
            .iter()
            .any(|c| fan_group(&c.id) == FanGroup::Motherboard);
        page.when(board_missing, |page| {
            page.child(Alert::warning(
                "no-board-fans",
                "Motherboard fans not detected: run the service as administrator with PawnIO installed.",
            ))
        })
        .child(
            h_flex()
                .items_start()
                .gap_4()
                .child(self.channel_list(cx))
                .children(editor),
        )
        .into_any_element()
    }

    /// "All fans: Silent | Balanced | Performance" in the page header.
    fn preset_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let channels = self.connected_channels();
        let current = self
            .state
            .draft
            .as_ref()
            .and_then(|d| current_preset(d, &channels));
        let none = !channels.iter().any(|c| !is_pump(c));
        let group = ButtonGroup::new("presets")
            .small()
            .outline()
            .disabled(none || self.is_pending())
            .children(PRESETS.iter().enumerate().map(|(i, (label, _))| {
                Button::new(("preset", i))
                    .label(*label)
                    .selected(current == Some(i))
                    .tooltip("Applies to all fans (pumps keep their safe settings)")
            }))
            .on_click(cx.listener(|app, clicks: &Vec<usize>, _, cx| {
                if let Some((_, preset)) = clicks.first().and_then(|&i| PRESETS.get(i)) {
                    app.apply_preset_all(*preset, cx);
                }
            }));
        h_flex().gap_2().child(muted("All fans", cx)).child(group)
    }

    fn channel_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut list = section(IconName::Fan, "Channels", cx)
            .w(px(300.))
            .flex_none();
        // A header at 0 RPM left in Auto probably has nothing plugged in: collapsed.
        let (connected, unconnected): (Vec<_>, Vec<_>) = self
            .state
            .channels
            .iter()
            .enumerate()
            .partition(|(_, c)| self.state.is_connected(c));
        for group in FanGroup::ALL {
            let rows: Vec<_> = connected
                .iter()
                .filter(|(_, c)| fan_group(&c.id) == group)
                .collect();
            if rows.is_empty() {
                continue;
            }
            list = list.child(group_label(group.title(), cx).pt_1());
            for (i, channel) in rows {
                list = list.child(self.fan_row(*i, channel, cx));
            }
        }
        if !unconnected.is_empty() {
            let open = self.fans.show_unconnected;
            list = list.child(
                disclosure("unconnected-toggle", open, cx)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .on_click(cx.listener(|app, _, _, cx| {
                        app.fans.show_unconnected = !app.fans.show_unconnected;
                        cx.notify();
                    }))
                    .child(format!("NOT CONNECTED ({})", unconnected.len())),
            );
            if open {
                for (i, channel) in &unconnected {
                    list = list.child(self.fan_row(*i, channel, cx));
                }
            }
        }
        list
    }

    /// Channel row: icon, name, speed, duty bar, mode (clickable: selection).
    fn fan_row(&self, i: usize, channel: &FanChannel, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let applied = self.state.applied_duty(&channel.id);
        let mode = self
            .state
            .draft
            .as_ref()
            .map(|d| settings_for(d, &channel.id).mode)
            .unwrap_or(FanMode::Auto);
        let duty = match applied {
            Some(Some(duty)) => format!("{duty} %"),
            _ => String::new(),
        };
        let rpm = format_rpm(channel_rpm(channel, self.state.readings()));
        let active = self.fans.selected.as_deref() == Some(channel.id.as_str());
        let pump = is_pump(channel);
        let id = channel.id.clone();
        selectable_row(("fan-row", i), active, cx)
            .on_click(cx.listener(move |app, _, _, cx| {
                app.fans.selected = Some(id.clone());
                app.fans.curve.dragging = None;
                cx.notify();
            }))
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(
                                Icon::new(if pump {
                                    IconName::Droplets
                                } else {
                                    IconName::Fan
                                })
                                .small()
                                .text_color(theme.muted_foreground),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .child(display_name(self.state.draft.as_ref(), channel)),
                            ),
                    )
                    .child(div().flex_none().text_sm().font_semibold().child(rpm)),
            )
            .child(duty_bar(applied.flatten(), cx))
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        h_flex()
                            .gap_1()
                            .child(mode_tag(&mode))
                            .when(pump, |row| row.child(pump_tag())),
                    )
                    .child(duty),
            )
    }

    fn fan_editor(
        &mut self,
        channel: &FanChannel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(controls) = self.controls.as_ref().map(|c| c.fans.clone()) else {
            return div().into_any_element();
        };
        let settings = self.selected_settings().unwrap_or_default();
        self.sync_fan_controls(&controls, channel, &settings, window, cx);

        let mode = &settings.mode;
        let mode_index = match mode {
            FanMode::Auto => 0,
            FanMode::Fixed { .. } => 1,
            FanMode::Curve { .. } => 2,
        };
        let modes = segmented("fan-mode", MODES.map(|(label, _, _)| label), mode_index).on_click(
            cx.listener(|app, clicks: &Vec<usize>, _, cx| {
                if let Some((_, key, _)) = clicks.first().and_then(|&i| MODES.get(i)) {
                    app.set_mode(key, cx);
                }
            }),
        );

        let customized = self
            .state
            .draft
            .as_ref()
            .and_then(|d| d.active())
            .is_some_and(|p| p.fans.contains_key(&channel.id));
        let header = h_flex()
            .justify_between()
            .gap_4()
            .child(
                v_flex()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .font_semibold()
                            .child(display_name(self.state.draft.as_ref(), channel))
                            .when(is_pump(channel), |row| row.child(pump_tag())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(channel.id.clone()),
                    ),
            )
            .child(
                Button::new("reset-fan")
                    .small()
                    .ghost()
                    .icon(IconName::RotateCcw)
                    .label("Reset this fan")
                    .tooltip("Back to Auto (BIOS)")
                    .disabled(!customized)
                    .on_click(cx.listener(|app, _, _, cx| app.reset_selected_fan(cx))),
            );
        let general = card(cx)
            .child(header)
            .child(field(
                "Name",
                div()
                    .w(px(260.))
                    .child(Input::new(&controls.name_input).small()),
                cx,
            ))
            .child(setting("Mode", MODES[mode_index].2, modes, cx))
            .when_some(
                match mode {
                    FanMode::Fixed { duty } => Some(*duty),
                    _ => None,
                },
                |s, duty| {
                    s.child(field(
                        format!("Speed: {duty} %"),
                        div().w(px(260.)).child(Slider::new(&controls.fixed_slider)),
                        cx,
                    ))
                },
            );

        let curve = match mode {
            FanMode::Curve { source, points } => {
                Some(self.curve_section(&controls, channel, source, points, settings.avoid, cx))
            }
            _ => None,
        };
        let advanced = (!matches!(mode, FanMode::Auto))
            .then(|| self.advanced_section(&controls, &settings, is_pump(channel), cx));

        v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(general)
            .children(curve)
            .children(advanced)
            .into_any_element()
    }

    fn curve_section(
        &self,
        controls: &FanControls,
        channel: &FanChannel,
        source: &TempSource,
        points: &[CurvePoint],
        avoid: Option<DutyRange>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current_temp = resolve_source(source, self.state.readings());
        // Marker: duty actually applied, otherwise the curve value.
        let marker = current_temp.map(|temp| {
            let duty = self
                .state
                .applied_duty(&channel.id)
                .flatten()
                .unwrap_or_else(|| evaluate_curve(points, temp));
            (temp, f32::from(duty))
        });
        let unit = self.prefs.temperature_unit;
        let status = match marker {
            Some((temp, duty)) => format!("Now {}, fan at {duty:.0} %", unit.format(temp, 1)),
            None => "Source temperature unavailable".into(),
        };
        let handlers = CurveHandlers {
            down: Self::curve_down,
            right: Self::curve_right,
            moved: Self::curve_move,
            up: Self::curve_up,
            up_out: Self::curve_up,
        };
        section(IconName::ChartLine, "Curve", cx)
            .child(field(
                "Temperature source",
                Select::new(&controls.source_select)
                    .id("curve-source")
                    .small()
                    .placeholder("Choose a sensor")
                    .w(px(260.)),
                cx,
            ))
            .child(
                self.fans
                    .curve
                    .render(points, avoid, marker, handlers, unit, cx),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap_4()
                    .child(curve_editor::help_text(cx))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .when(marker.is_some(), |row| {
                                row.child(div().size(px(8.)).rounded_full().bg(hex(MARKER)))
                            })
                            .child(status),
                    ),
            )
            .into_any_element()
    }

    fn advanced_section(
        &self,
        controls: &FanControls,
        settings: &FanSettings,
        pump: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.fans.show_advanced;
        let toggle = disclosure("advanced-toggle", open, cx)
            .text_sm()
            .on_click(cx.listener(|app, _, _, cx| {
                app.fans.show_advanced = !app.fans.show_advanced;
                cx.notify();
            }))
            .child(Icon::new(IconName::SlidersHorizontal).small())
            .child("Advanced");
        let section = card(cx).child(toggle);
        if !open {
            return section.into_any_element();
        }
        let curve = matches!(settings.mode, FanMode::Curve { .. });
        let zero_rpm = settings.zero_rpm_below.filter(|_| !pump);
        let unit = self.prefs.temperature_unit;
        section
            .child(setting(
                format!("Minimum speed: {} %", settings.min_duty),
                "The fan never runs slower than this.",
                div().w(px(260.)).child(Slider::new(&controls.min_slider)),
                cx,
            ))
            .when(pump, |s| {
                s.child(muted(
                    format!(
                        "A pump must never stop: the service enforces at least {PUMP_MIN_DUTY} %."
                    ),
                    cx,
                ))
            })
            .when(curve, |s| {
                s.child(setting(
                    "Stop the fan when cool",
                    "Below a temperature the fan stops completely (0 RPM).",
                    Switch::new("zero-rpm")
                        .checked(zero_rpm.is_some())
                        .disabled(pump)
                        .on_click(cx.listener(|app, checked: &bool, _, cx| {
                            let value = checked.then_some(40.0);
                            app.edit_fan(cx, |s| s.zero_rpm_below = value);
                        })),
                    cx,
                ))
            })
            .when_some(zero_rpm.filter(|_| curve), |s, below| {
                s.child(field(
                    "Stop below",
                    stepper(
                        "zero-rpm-temp",
                        unit.format(below, 0),
                        cx.listener(|app, _, _, cx| {
                            app.edit_fan(cx, |s| {
                                s.zero_rpm_below = s.zero_rpm_below.map(|t| (t - 1.0).max(20.0))
                            });
                        }),
                        cx.listener(|app, _, _, cx| {
                            app.edit_fan(cx, |s| {
                                s.zero_rpm_below = s.zero_rpm_below.map(|t| (t + 1.0).min(80.0))
                            });
                        }),
                    ),
                    cx,
                ))
            })
            .when(curve, |s| {
                s.child(setting(
                    "Temperature tolerance",
                    "Ignore temperature changes smaller than this, so the speed does not keep changing.",
                    stepper(
                        "hysteresis",
                        unit.format_delta(settings.hysteresis, 1),
                        cx.listener(|app, _, _, cx| {
                            app.edit_fan(cx, |s| s.hysteresis = (s.hysteresis - 0.5).max(0.0));
                        }),
                        cx.listener(|app, _, _, cx| {
                            app.edit_fan(cx, |s| s.hysteresis = (s.hysteresis + 0.5).min(10.0));
                        }),
                    ),
                    cx,
                ))
                .child(setting(
                    "Max increase per update",
                    "Limits how fast the fan speeds up.",
                    stepper(
                        "step-up",
                        format!("{} %", settings.step_up),
                        cx.listener(|app, _, _, cx| {
                            app.edit_fan(cx, |s| s.step_up = s.step_up.saturating_sub(1).max(1));
                        }),
                        cx.listener(|app, _, _, cx| {
                            app.edit_fan(cx, |s| s.step_up = (s.step_up + 1).min(100));
                        }),
                    ),
                    cx,
                ))
                .child(setting(
                    "Max decrease per update",
                    "Limits how fast the fan slows down.",
                    stepper(
                        "step-down",
                        format!("{} %", settings.step_down),
                        cx.listener(|app, _, _, cx| {
                            app.edit_fan(cx, |s| {
                                s.step_down = s.step_down.saturating_sub(1).max(1)
                            });
                        }),
                        cx.listener(|app, _, _, cx| {
                            app.edit_fan(cx, |s| s.step_down = (s.step_down + 1).min(100));
                        }),
                    ),
                    cx,
                ))
                .child(setting(
                    "Minimum time between changes",
                    "The speed changes at most this often.",
                    stepper(
                        "response",
                        format!("{} ms", settings.response_ms),
                        cx.listener(|app, _, _, cx| {
                            app.edit_fan(cx, |s| {
                                s.response_ms = s.response_ms.saturating_sub(250)
                            });
                        }),
                        cx.listener(|app, _, _, cx| {
                            app.edit_fan(cx, |s| {
                                s.response_ms = (s.response_ms + 250).min(10_000)
                            });
                        }),
                    ),
                    cx,
                ))
                .child(self.avoid_settings(settings.avoid, cx))
            })
            .into_any_element()
    }

    /// "Skip a speed range" switch and its from / to steppers.
    fn avoid_settings(&self, avoid: Option<DutyRange>, cx: &mut Context<Self>) -> Div {
        v_flex()
            .gap_3()
            .child(setting(
                "Skip a speed range",
                "Some fans hum or vibrate at certain speeds: the curve jumps over this range.",
                Switch::new("avoid-band")
                    .checked(avoid.is_some())
                    .on_click(cx.listener(|app, checked: &bool, _, cx| {
                        let value = checked.then_some(DEFAULT_AVOID);
                        app.edit_fan(cx, |s| s.avoid = value);
                    })),
                cx,
            ))
            .when_some(avoid, |s, band| {
                let edit = |change: fn(&mut DutyRange)| {
                    cx.listener(move |app: &mut HubApp, _, _, cx| {
                        app.edit_fan(cx, |s| {
                            if let Some(band) = s.avoid.as_mut() {
                                change(band);
                            }
                        })
                    })
                };
                s.child(field(
                    "From",
                    stepper(
                        "avoid-min",
                        format!("{} %", band.min),
                        edit(|b| b.min = b.min.saturating_sub(1)),
                        edit(|b| b.min = (b.min + 1).min(b.max - 1)),
                    ),
                    cx,
                ))
                .child(field(
                    "To",
                    stepper(
                        "avoid-max",
                        format!("{} %", band.max),
                        edit(|b| b.max = (b.max - 1).max(b.min + 1)),
                        edit(|b| b.max = (b.max + 1).min(100)),
                    ),
                    cx,
                ))
            })
    }
}

/// Clickable "chevron + label" row toggling a collapsed block.
fn disclosure(id: &'static str, open: bool, cx: &Context<HubApp>) -> gpui_kit::Stateful<Div> {
    let theme = cx.theme();
    h_flex()
        .id(id)
        .gap_2()
        .px_2()
        .py_1()
        .rounded(px(6.))
        .cursor_pointer()
        .font_semibold()
        .hover(|row| row.bg(theme.secondary).text_color(theme.foreground))
        .child(
            Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall(),
        )
}

/// Setting row: label with a muted description on the left, control on the right.
fn setting(
    label: impl Into<SharedString>,
    description: &'static str,
    control: impl IntoElement,
    cx: &Context<HubApp>,
) -> Div {
    h_flex()
        .justify_between()
        .gap_4()
        .child(
            v_flex()
                .min_w_0()
                .child(div().text_sm().child(label.into()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                ),
        )
        .child(div().flex_none().child(control))
}

/// Mode badge: highlighted when the app drives the fan.
fn mode_tag(mode: &FanMode) -> Tag {
    let tag = match mode {
        FanMode::Auto => Tag::secondary().outline(),
        _ => Tag::info(),
    };
    tag.small().child(mode_label(mode))
}

/// Pump badge (minimum duty enforced by the service).
fn pump_tag() -> Tag {
    Tag::secondary()
        .small()
        .child(format!("Pump, min {PUMP_MIN_DUTY} %"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(id: &str, label: &str) -> FanChannel {
        FanChannel {
            id: id.into(),
            label: label.into(),
            rpm_sensor: None,
        }
    }

    #[test]
    fn preset_applies_to_all_fans_but_not_pumps() {
        let mut config = Config::default();
        let channels = [
            channel("nct/pwm1", "CPU Fan"),
            channel("nct/pwm2", "Pump Fan"),
            channel("gpu0/fan0", "GPU Fan"),
        ];
        let custom = TempSource::Sensor { id: "gpu".into() };
        let pump = FanSettings {
            mode: FanMode::Fixed { duty: 70 },
            ..FanSettings::default()
        };
        if let Some(s) = settings_mut(&mut config, "nct/pwm2") {
            *s = pump.clone();
        }
        if let Some(s) = settings_mut(&mut config, "gpu0/fan0") {
            s.mode = FanMode::Curve {
                source: custom.clone(),
                points: Preset::Performance.points(),
            };
        }
        let cpu = TempSource::Sensor { id: "cpu".into() };
        assert!(apply_preset(&mut config, &channels, Preset::Silent, &cpu));
        for (id, source) in [("nct/pwm1", &cpu), ("gpu0/fan0", &custom)] {
            let s = settings_for(&config, id);
            assert!(Preset::Silent.matches(&s), "{id}");
            assert!(matches!(&s.mode, FanMode::Curve { source: src, .. } if src == source));
        }
        assert_eq!(settings_for(&config, "nct/pwm2"), pump);
        assert_eq!(current_preset(&config, &channels), Some(0));
        assert!(!apply_preset(&mut config, &channels, Preset::Silent, &cpu));
        assert_eq!(current_preset(&config, &[channels[1].clone()]), None);
    }
}
