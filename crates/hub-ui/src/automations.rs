//! "Automations" page: profile switching by running app and by time of day.
//! Rules are edited in the draft and sent with "Apply"; the service evaluates them.

use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AnyWindowHandle, AppContext as _, ClickEvent, Context, ElementId, Entity,
    IntoElement, ParentElement as _, Styled as _, Window, div, px,
};
use hub_core::config::{Config, DAY_MINUTES, ProcessRule, ScheduleRule};

use crate::app::HubApp;
use crate::widgets::{empty_state, loading, muted, page_header, section, stepper};

/// Schedule bounds move by this many minutes.
const TIME_STEP: u16 = 30;

/// Page state: the app name input, tied to the window that created it.
#[derive(Default)]
pub struct AutomationsPage {
    exe_input: Option<(AnyWindowHandle, Entity<InputState>)>,
    /// Profile forced right now by a rule, as reported by the service.
    pub active: Option<String>,
}

/// "23:00" from minutes since midnight.
fn format_time(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Moves a time by `delta` minutes, wrapping around midnight.
fn shift_time(minutes: u16, delta: i32) -> u16 {
    (i32::from(minutes) + delta).rem_euclid(i32::from(DAY_MINUTES)) as u16
}

/// Which rule list a row belongs to.
#[derive(Clone, Copy)]
enum Rule {
    Process(usize),
    Schedule(usize),
}

impl Rule {
    fn profile_mut(self, config: &mut Config) -> Option<&mut String> {
        let rules = &mut config.automations;
        match self {
            Self::Process(i) => rules.process_rules.get_mut(i).map(|r| &mut r.profile),
            Self::Schedule(i) => rules.schedules.get_mut(i).map(|r| &mut r.profile),
        }
    }

    fn remove(self, config: &mut Config) {
        let rules = &mut config.automations;
        match self {
            Self::Process(i) if i < rules.process_rules.len() => {
                rules.process_rules.remove(i);
            }
            Self::Schedule(i) if i < rules.schedules.len() => {
                rules.schedules.remove(i);
            }
            _ => {}
        }
    }

    fn id(self, part: &str) -> ElementId {
        let (kind, i) = match self {
            Self::Process(i) => ("process", i),
            Self::Schedule(i) => ("schedule", i),
        };
        ElementId::NamedInteger(format!("{kind}-{part}").into(), i as u64)
    }
}

impl HubApp {
    pub fn render_automations(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let header = page_header(
            IconName::Zap,
            "Automations",
            "Switch profiles when an app runs or at set times",
            cx,
        );
        let Some(draft) = self.state.draft.clone() else {
            return v_flex()
                .gap_4()
                .child(header)
                .child(loading("Waiting for the service configuration...", cx))
                .into_any_element();
        };
        let input = self.exe_input(window, cx);
        let rules = &draft.automations;
        let profiles: Vec<String> = draft.profiles.iter().map(|p| p.name.clone()).collect();

        let apps = rules.process_rules.iter().enumerate().map(|(i, rule)| {
            let name = div()
                .flex_1()
                .text_sm()
                .font_semibold()
                .child(rule.exe.clone());
            self.rule_row(Rule::Process(i), name, &rule.profile, &profiles, cx)
        });
        let apps: Vec<_> = apps.collect();
        let times = rules.schedules.iter().enumerate().map(|(i, rule)| {
            let rule_ref = Rule::Schedule(i);
            let time = |part: &str, value: u16, start: bool, cx: &mut Context<Self>| {
                let edit = move |delta: i32| {
                    move |app: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                        app.edit_config(cx, |config| {
                            if let Some(rule) = config.automations.schedules.get_mut(i) {
                                let bound = if start {
                                    &mut rule.start
                                } else {
                                    &mut rule.end
                                };
                                *bound = shift_time(*bound, delta);
                            }
                        });
                    }
                };
                let id = format!("schedule-{i}-{part}");
                stepper(
                    &id,
                    format_time(value),
                    cx.listener(edit(-i32::from(TIME_STEP))),
                    cx.listener(edit(i32::from(TIME_STEP))),
                )
            };
            let from = time("start", rule.start, true, cx);
            let to = time("end", rule.end, false, cx);
            let range = h_flex()
                .flex_1()
                .gap_2()
                .items_center()
                .child(from)
                .child(muted("to", cx))
                .child(to);
            self.rule_row(rule_ref, range, &rule.profile, &profiles, cx)
        });
        let times: Vec<_> = times.collect();
        let nothing = apps.is_empty() && times.is_empty();

        let add_app = h_flex()
            .gap_2()
            .child(div().flex_1().child(Input::new(&input).small()))
            .child(
                Button::new("add-process-rule")
                    .small()
                    .outline()
                    .icon(IconName::Plus)
                    .label("Add app")
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.add_process_rule(&input, window, cx);
                    })),
            );
        let add_time = Button::new("add-schedule")
            .small()
            .outline()
            .icon(IconName::Plus)
            .label("Add time range")
            .on_click(cx.listener(|app, _, _, cx| {
                app.edit_config(cx, |config| {
                    let profile = config
                        .profiles
                        .iter()
                        .find(|p| p.name == "Silent")
                        .map_or(config.active_profile.clone(), |p| p.name.clone());
                    config.automations.schedules.push(ScheduleRule {
                        start: 23 * 60,
                        end: 7 * 60,
                        profile,
                    });
                });
            }));

        v_flex()
            .gap_4()
            .max_w(px(760.))
            .child(header)
            .children(self.automations.active.as_ref().map(|profile| {
                Alert::info("automation-active", format!("Active now: {profile} (set by a rule)"))
            }))
            .children(nothing.then(|| {
                empty_state(
                    IconName::Zap,
                    "No automations yet",
                    "Add an app or a time range below. An app rule wins over a time range, \
                     and both win over the profile you pick by hand.",
                    cx,
                )
            }))
            .child(
                section(IconName::Play, "When an app runs", cx)
                    .child(muted(
                        "The profile is used while the app is open, then the previous one comes back.",
                        cx,
                    ))
                    .children(apps)
                    .child(add_app),
            )
            .child(
                section(IconName::Clock, "Schedule", cx)
                    .child(muted(
                        "Local time. A range may cross midnight (23:00 to 07:00).",
                        cx,
                    ))
                    .children(times)
                    .child(h_flex().child(add_time)),
            )
            .into_any_element()
    }

    /// App name input, recreated when the window changes.
    fn exe_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let handle = window.window_handle();
        match &self.automations.exe_input {
            Some((owner, input)) if *owner == handle => input.clone(),
            _ => {
                let input = cx.new(|cx| {
                    InputState::new(window, cx).placeholder("App name, for example game.exe")
                });
                self.automations.exe_input = Some((handle, input.clone()));
                input
            }
        }
    }

    fn add_process_rule(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let exe = input.read(cx).value().trim().to_owned();
        if exe.is_empty() {
            return self.notify_error("Type the app name first, for example game.exe.", cx);
        }
        self.edit_config(cx, |config| {
            let profile = config
                .profiles
                .iter()
                .find(|p| p.name == "Performance")
                .map_or(config.active_profile.clone(), |p| p.name.clone());
            config
                .automations
                .process_rules
                .push(ProcessRule { exe, profile });
        });
        input.update(cx, |state, cx| state.set_value("", window, cx));
    }

    /// Rule row: `content` on the left, then the profile dropdown and the delete button.
    fn rule_row(
        &self,
        rule: Rule,
        content: impl IntoElement,
        profile: &str,
        profiles: &[String],
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let hub = cx.entity();
        let names = profiles.to_vec();
        let current = profile.to_owned();
        let missing = !profiles.iter().any(|p| p == profile);
        let label = if missing {
            format!("{profile} (deleted)")
        } else {
            profile.to_owned()
        };
        let dropdown = Button::new(rule.id("profile"))
            .small()
            .outline()
            .label(label)
            .dropdown_menu(move |menu, _, _| {
                names.iter().fold(menu, |menu, name| {
                    let hub = hub.clone();
                    let chosen = name.clone();
                    menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(*name == current)
                            .on_click(move |_, _, cx| {
                                let chosen = chosen.clone();
                                hub.update(cx, |app, cx| {
                                    app.edit_config(cx, |config| {
                                        if let Some(profile) = rule.profile_mut(config) {
                                            *profile = chosen;
                                        }
                                    });
                                });
                            }),
                    )
                })
            });
        let theme = cx.theme();
        h_flex()
            .gap_3()
            .items_center()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(content)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::ChevronRight).text_color(theme.muted_foreground))
                    .child(dropdown)
                    .child(
                        Button::new(rule.id("delete"))
                            .small()
                            .ghost()
                            .icon(IconName::Trash)
                            .tooltip("Remove")
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.edit_config(cx, |config| rule.remove(config));
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_formatting_and_wrapping() {
        assert_eq!(format_time(23 * 60), "23:00");
        assert_eq!(format_time(7 * 60 + 30), "07:30");
        assert_eq!(shift_time(23 * 60 + 30, 30), 0);
        assert_eq!(shift_time(0, -30), 23 * 60 + 30);
    }
}
