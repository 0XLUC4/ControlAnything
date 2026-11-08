//! "Settings" page: profiles (create, rename, delete, import/export),
//! global daemon settings and application preferences.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, PathPromptOptions, SharedString,
    Styled as _, div, px,
};
use hub_core::config::Config;
use hub_core::ipc::ClientMsg;

use crate::app::HubApp;
use crate::hotkeys::hotkey_label;
use crate::prefs::{Prefs, TempUnit, set_autostart};
use crate::state::{Notice, delete_profile, duplicate_active, rename_active};
use crate::widgets::{
    GREEN, field, loading, muted, page_header, section, segmented, solid_tag, stepper,
};

/// How long a profile switch shows as loading without an answer from the service.
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Settings page state: actions waiting on a file dialog or on the service.
#[derive(Default)]
pub struct SettingsPage {
    /// Open file dialog: `Export` or `Import`.
    dialog: Option<Dialog>,
    /// Profile switch sent to the service, and when.
    activating: Option<(String, Instant)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialog {
    Export,
    Import,
}

impl SettingsPage {
    /// The switch to `name` was sent less than [`ACTIVATE_TIMEOUT`] ago.
    fn is_activating(&self, name: &str, now: Instant) -> bool {
        self.activating.as_ref().is_some_and(|(target, at)| {
            target == name && now.saturating_duration_since(*at) < ACTIVATE_TIMEOUT
        })
    }
}

impl HubApp {
    /// Edits the preferences, applies their effects (autostart, hotkeys) then saves them.
    fn edit_prefs(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Prefs)) {
        let mut prefs = self.prefs.clone();
        edit(&mut prefs);
        if prefs == self.prefs {
            return;
        }
        if prefs.launch_at_startup != self.prefs.launch_at_startup
            && let Err(message) = set_autostart(prefs.launch_at_startup)
        {
            prefs.launch_at_startup = self.prefs.launch_at_startup;
            self.notify_error(message, cx);
        }
        if let Some(hotkeys) = self.hotkeys.as_mut() {
            hotkeys.set_enabled(prefs.hotkeys);
        }
        if let Err(message) = prefs.save() {
            self.notify_error(format!("Preferences not saved: {message}"), cx);
        }
        self.prefs = prefs;
        cx.notify();
    }

    /// Applies a profile operation to the draft; the error is shown in the banner.
    fn edit_profiles(
        &mut self,
        cx: &mut Context<Self>,
        op: impl FnOnce(&mut Config, &str) -> Result<(), String>,
    ) {
        let name = self
            .controls
            .as_ref()
            .map(|c| c.profile_name.read(cx).value().to_string())
            .unwrap_or_default();
        let Some(draft) = self.state.draft.as_mut() else {
            return;
        };
        if let Err(message) = op(draft, &name) {
            self.notify_error(message, cx);
        }
        cx.notify();
    }

    pub fn edit_config(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Config)) {
        if let Some(draft) = self.state.draft.as_mut() {
            edit(draft);
            cx.notify();
        }
    }

    /// Exports the daemon config (profiles included) to a TOML file chosen by the user.
    fn export_config(&mut self, cx: &mut Context<Self>) {
        let Some(config) = self.state.config.clone() else {
            return;
        };
        let text = match config.to_toml() {
            Ok(text) => text,
            Err(err) => return self.notify_error(err.to_string(), cx),
        };
        let directory = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let chosen = cx.prompt_for_new_path(&directory, Some("controlanything.toml"));
        self.settings.dialog = Some(Dialog::Export);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let notice = match chosen.await {
                Ok(Ok(Some(path))) => Some(match std::fs::write(&path, text) {
                    Ok(()) => Notice::Info(format!("Configuration exported: {}", path.display())),
                    Err(err) => Notice::Error(format!("Export failed: {err}")),
                }),
                Ok(Err(err)) => Some(Notice::Error(format!("Dialog unavailable: {err}"))),
                // Cancelled by the user.
                Ok(Ok(None)) | Err(_) => None,
            };
            let _ = this.update(cx, |app, cx| {
                app.settings.dialog = None;
                if notice.is_some() {
                    app.state.notice = notice;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Imports a TOML file and sends it to the daemon (which validates it again).
    fn import_config(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        self.settings.dialog = Some(Dialog::Import);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let chosen = chosen.await;
            let path = this
                .update(cx, |app, cx| {
                    app.settings.dialog = None;
                    cx.notify();
                    match chosen {
                        Ok(Ok(Some(paths))) => paths.into_iter().next(),
                        Ok(Err(err)) => {
                            app.notify_error(format!("Dialog unavailable: {err}"), cx);
                            None
                        }
                        // Cancelled by the user.
                        Ok(Ok(None)) | Err(_) => None,
                    }
                })
                .ok()
                .flatten();
            let Some(path) = path else {
                return;
            };
            let parsed = std::fs::read_to_string(&path)
                .map_err(|err| format!("Cannot read file: {err}"))
                .and_then(|text| Config::from_toml(&text).map_err(|err| err.to_string()))
                .and_then(|mut config| {
                    config.validate().map_err(|err| err.to_string())?;
                    Ok(config)
                });
            let _ = this.update(cx, |app, cx| match parsed {
                Ok(config) => {
                    app.state.start_pending();
                    app.state.notice = Some(Notice::Info(format!(
                        "Configuration imported: {}",
                        path.display()
                    )));
                    app.send(ClientMsg::SetConfig { config });
                    cx.notify();
                }
                Err(message) => app.notify_error(message, cx),
            });
        })
        .detach();
    }

    pub fn render_settings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let header = page_header(
            IconName::Settings,
            "Settings",
            "Profiles, safety and preferences",
            cx,
        );
        let app_prefs = self.app_section(cx);
        let (Some(draft), Some(config), Some(controls)) = (
            self.state.draft.as_ref(),
            self.state.config.as_ref(),
            self.controls.as_ref(),
        ) else {
            return v_flex()
                .gap_4()
                .max_w(px(760.))
                .child(header)
                .child(loading("Waiting for the service configuration...", cx))
                .child(app_prefs)
                .into_any_element();
        };
        let theme = cx.theme().clone();
        let unit = self.prefs.temperature_unit;
        let hotkeys_on = self.prefs.hotkeys && self.hotkeys.is_some();
        let now = Instant::now();

        let reset_profile = self.reset_profile_button("reset-profile-settings", cx);
        let rows = draft.profiles.iter().enumerate().map(|(i, profile)| {
            let name = profile.name.clone();
            let is_active = draft.active_profile == profile.name;
            // Only a profile known to the daemon can be activated directly.
            let known = config.profiles.iter().any(|p| p.name == profile.name);
            let activating = self.settings.is_activating(&name, now);
            let activate = name.clone();
            let delete = name.clone();
            // Hotkey based on the order of the profiles known to the daemon.
            let shortcut = config
                .profiles
                .iter()
                .position(|p| p.name == profile.name)
                .and_then(hotkey_label)
                .filter(|_| hotkeys_on);
            h_flex()
                .justify_between()
                .py_1()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().text_sm().font_semibold().child(name))
                        .when(is_active, |row| {
                            row.child(solid_tag(GREEN).small().child("Active"))
                        })
                        .when_some(shortcut, |row, keys| {
                            row.child(Tag::secondary().outline().small().child(keys))
                        }),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .when(!is_active && known, |row| {
                            row.child(
                                Button::new(("activate", i))
                                    .small()
                                    .outline()
                                    .icon(IconName::Play)
                                    .label("Activate")
                                    .loading(activating)
                                    .on_click(cx.listener(move |app, _, _, cx| {
                                        app.settings.activating =
                                            Some((activate.clone(), Instant::now()));
                                        app.activate_profile(activate.clone());
                                        cx.notify();
                                    })),
                            )
                        })
                        .child(
                            Button::new(("delete", i))
                                .small()
                                .ghost()
                                .icon(IconName::Trash)
                                .tooltip("Delete profile")
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    let target = delete.clone();
                                    app.edit_profiles(cx, |c, _| delete_profile(c, &target));
                                })),
                        ),
                )
        });

        let dialog = self.settings.dialog;
        let profiles = section(IconName::Layers, "Profiles", cx)
            .children(rows)
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        div()
                            .w(px(240.))
                            .child(Input::new(&controls.profile_name).small()),
                    )
                    .child(
                        Button::new("duplicate")
                            .small()
                            .outline()
                            .icon(IconName::Copy)
                            .label("Duplicate active")
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.edit_profiles(cx, duplicate_active);
                            })),
                    )
                    .child(
                        Button::new("rename")
                            .small()
                            .outline()
                            .icon(IconName::Pencil)
                            .label("Rename active")
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.edit_profiles(cx, rename_active);
                            })),
                    ),
            )
            .child(Separator::horizontal())
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("export")
                            .small()
                            .outline()
                            .icon(IconName::FileDown)
                            .label("Export (.toml)...")
                            .loading(dialog == Some(Dialog::Export))
                            .on_click(cx.listener(|app, _, _, cx| app.export_config(cx))),
                    )
                    .child(
                        Button::new("import")
                            .small()
                            .outline()
                            .icon(IconName::FolderOpen)
                            .label("Import (.toml)...")
                            .loading(dialog == Some(Dialog::Import) || self.is_pending())
                            .on_click(cx.listener(|app, _, _, cx| app.import_config(cx))),
                    )
                    .child(reset_profile),
            )
            .child(
                muted(
                    "Reset profile: every fan of the active profile goes back to Auto (BIOS).",
                    cx,
                )
                .text_xs(),
            );

        let failsafe = &draft.failsafe;
        let safety = section(IconName::ShieldAlert, "Safety (failsafe)", cx)
            .child(muted(
                "Above these thresholds, all controlled fans go to 100 %.",
                cx,
            ))
            .child(field(
                "CPU critical threshold",
                stepper(
                    "cpu-critical",
                    unit.format(failsafe.cpu_critical, 0),
                    cx.listener(|app, _, _, cx| {
                        app.edit_config(cx, |c| {
                            c.failsafe.cpu_critical = (c.failsafe.cpu_critical - 1.0).max(60.0);
                        });
                    }),
                    cx.listener(|app, _, _, cx| {
                        app.edit_config(cx, |c| {
                            c.failsafe.cpu_critical = (c.failsafe.cpu_critical + 1.0).min(105.0);
                        });
                    }),
                ),
                cx,
            ))
            .child(field(
                "GPU critical threshold",
                stepper(
                    "gpu-critical",
                    unit.format(failsafe.gpu_critical, 0),
                    cx.listener(|app, _, _, cx| {
                        app.edit_config(cx, |c| {
                            c.failsafe.gpu_critical = (c.failsafe.gpu_critical - 1.0).max(60.0);
                        });
                    }),
                    cx.listener(|app, _, _, cx| {
                        app.edit_config(cx, |c| {
                            c.failsafe.gpu_critical = (c.failsafe.gpu_critical + 1.0).min(100.0);
                        });
                    }),
                ),
                cx,
            ));

        let polling = section(IconName::Timer, "Sampling", cx)
            .child(field(
                "Polling interval",
                stepper(
                    "poll",
                    format!("{} ms", draft.poll_interval_ms),
                    cx.listener(|app, _, _, cx| {
                        app.edit_config(cx, |c| {
                            c.poll_interval_ms = c.poll_interval_ms.saturating_sub(100).max(500);
                        });
                    }),
                    cx.listener(|app, _, _, cx| {
                        app.edit_config(cx, |c| {
                            c.poll_interval_ms = (c.poll_interval_ms + 100).min(2000);
                        });
                    }),
                ),
                cx,
            ))
            .child(
                muted(
                    "How often the service reads sensors and updates fans. Lower reacts faster.",
                    cx,
                )
                .text_xs(),
            );

        v_flex()
            .gap_4()
            .max_w(px(760.))
            .child(header)
            .child(profiles)
            .child(safety)
            .child(polling)
            .child(app_prefs)
            .into_any_element()
    }

    /// Application preferences (`ui.toml` file, independent of the daemon).
    fn app_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let prefs = &self.prefs;
        let toggle = |id: &'static str,
                      checked: bool,
                      apply: fn(&mut Prefs, bool),
                      cx: &mut Context<Self>| {
            Switch::new(id).checked(checked).on_click(cx.listener(
                move |app, checked: &bool, _, cx| {
                    let checked = *checked;
                    app.edit_prefs(cx, |p| apply(p, checked));
                },
            ))
        };
        let selected = usize::from(prefs.temperature_unit == TempUnit::Fahrenheit);
        let units = segmented("units", ["°C", "°F"], selected).on_click(cx.listener(
            |app, clicks: &Vec<usize>, _, cx| {
                let unit = if clicks.first() == Some(&1) {
                    TempUnit::Fahrenheit
                } else {
                    TempUnit::Celsius
                };
                app.edit_prefs(cx, |p| p.temperature_unit = unit);
            },
        ));
        let hotkeys_label: SharedString = if self.hotkeys.is_some() {
            "Global hotkeys Ctrl+Shift+F1...F4 (profiles 1 to 4)".into()
        } else {
            "Global hotkeys (unavailable on this system)".into()
        };
        section(IconName::Settings2, "Application", cx)
            .child(field(
                "Launch ControlAnything at login",
                toggle("launch-at-startup", prefs.launch_at_startup, |p, v| p.launch_at_startup = v, cx),
                cx,
            ))
            .child(field(
                "At login, stay in the system tray",
                toggle("start-minimized", prefs.start_minimized, |p, v| p.start_minimized = v, cx),
                cx,
            ))
            .child(field(
                "Closing the window minimizes to the system tray",
                toggle("close-to-tray", prefs.close_to_tray, |p, v| p.close_to_tray = v, cx),
                cx,
            ))
            .child(field(
                hotkeys_label,
                toggle("hotkeys", prefs.hotkeys, |p, v| p.hotkeys = v, cx),
                cx,
            ))
            .child(field("Temperature unit", units, cx))
            .child(Alert::info(
                "service-info",
                "Quitting ControlAnything does not stop fan control: the controlanythingd service keeps driving the fans.",
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_shows_loading_until_timeout() {
        let now = Instant::now();
        let page = SettingsPage {
            dialog: None,
            activating: Some(("Silent".into(), now)),
        };
        assert!(page.is_activating("Silent", now));
        assert!(!page.is_activating("Gaming", now));
        assert!(!page.is_activating("Silent", now + ACTIVATE_TIMEOUT));
    }
}
