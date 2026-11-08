//! Page « Paramètres » : profils (création, renommage, suppression, import/export),
//! réglages globaux du daemon et préférences de l'application.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, PathPromptOptions, SharedString,
    Styled as _, div, px,
};
use hub_core::config::Config;
use hub_core::ipc::ClientMsg;

use crate::app::HubApp;
use crate::fans::waiting;
use crate::hotkeys::hotkey_label;
use crate::prefs::{Prefs, TempUnit, set_autostart};
use crate::state::{Notice, delete_profile, duplicate_active, rename_active};
use crate::widgets::{GREEN, choice, field, section, solid_tag, stepper};

impl HubApp {
    /// Modifie les préférences, applique leurs effets (démarrage auto, raccourcis) puis les enregistre.
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
            self.notify_error(format!("Préférences non enregistrées : {message}"), cx);
        }
        self.prefs = prefs;
        cx.notify();
    }

    /// Applique une opération de profil au brouillon ; l'erreur s'affiche dans le bandeau.
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

    /// Exporte la config du daemon (profils compris) dans un fichier TOML choisi par l'utilisateur.
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
        cx.spawn(async move |this, cx| {
            let notice = match chosen.await {
                Ok(Ok(Some(path))) => Some(match std::fs::write(&path, text) {
                    Ok(()) => Notice::Info(format!("Configuration exportée : {}", path.display())),
                    Err(err) => Notice::Error(format!("Export impossible : {err}")),
                }),
                Ok(Err(err)) => Some(Notice::Error(format!("Boîte de dialogue indisponible : {err}"))),
                // Annulé par l'utilisateur.
                Ok(Ok(None)) | Err(_) => None,
            };
            let _ = this.update(cx, |app, cx| {
                if notice.is_some() {
                    app.state.notice = notice;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Importe un fichier TOML et l'envoie au daemon (qui le revalide).
    fn import_config(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Importer".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match chosen.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(err)) => {
                    let _ = this.update(cx, |app, cx| {
                        app.notify_error(format!("Boîte de dialogue indisponible : {err}"), cx);
                    });
                    None
                }
                Ok(Ok(None)) | Err(_) => None,
            };
            let Some(path) = path else {
                return;
            };
            let parsed = std::fs::read_to_string(&path)
                .map_err(|err| format!("Lecture impossible : {err}"))
                .and_then(|text| Config::from_toml(&text).map_err(|err| err.to_string()))
                .and_then(|mut config| {
                    config.validate().map_err(|err| err.to_string())?;
                    Ok(config)
                });
            let _ = this.update(cx, |app, cx| match parsed {
                Ok(config) => {
                    app.state.pending_apply = true;
                    app.state.notice =
                        Some(Notice::Info(format!("Configuration importée : {}", path.display())));
                    app.send(ClientMsg::SetConfig { config });
                    cx.notify();
                }
                Err(message) => app.notify_error(message, cx),
            });
        })
        .detach();
    }

    pub fn render_settings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let app_prefs = self.app_section(cx);
        let (Some(draft), Some(config), Some(controls)) = (
            self.state.draft.as_ref(),
            self.state.config.as_ref(),
            self.controls.as_ref(),
        ) else {
            return v_flex()
                .gap_4()
                .max_w(px(760.))
                .child(waiting(cx))
                .child(app_prefs)
                .into_any_element();
        };
        let theme = cx.theme().clone();
        let unit = self.prefs.temperature_unit;
        let hotkeys_on = self.prefs.hotkeys && self.hotkeys.is_some();

        let reset_profile = self.reset_profile_button("reset-profile-settings", cx);
        let rows = draft.profiles.iter().enumerate().map(|(i, profile)| {
            let name = profile.name.clone();
            let is_active = draft.active_profile == profile.name;
            // Seul un profil connu du daemon peut être activé directement.
            let known = config.profiles.iter().any(|p| p.name == profile.name);
            let activate = name.clone();
            let delete = name.clone();
            // Raccourci selon l'ordre des profils connus du daemon.
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
                        .when(is_active, |row| row.child(solid_tag(GREEN).small().child("actif")))
                        .when_some(shortcut, |row, keys| {
                            row.child(
                                div()
                                    .text_xs()
                                    .px_1()
                                    .rounded(px(4.))
                                    .border_1()
                                    .border_color(theme.border)
                                    .text_color(theme.muted_foreground)
                                    .child(keys),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .when(!is_active && known, |row| {
                            row.child(
                                Button::new(("activate", i))
                                    .small()
                                    .outline()
                                    .icon(IconName::Play)
                                    .label("Activer")
                                    .on_click(cx.listener(move |app, _, _, _| {
                                        app.send(ClientMsg::SetProfile {
                                            name: activate.clone(),
                                        });
                                    })),
                            )
                        })
                        .child(
                            Button::new(("delete", i))
                                .small()
                                .danger()
                                .icon(IconName::Trash)
                                .label("Supprimer")
                                .on_click(cx.listener(move |app, _, _, cx| {
                                    let target = delete.clone();
                                    app.edit_profiles(cx, |c, _| delete_profile(c, &target));
                                })),
                        ),
                )
        });

        let profiles = section("Profils", cx)
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
                            .icon(IconName::Plus)
                            .label("Dupliquer le profil actif")
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.edit_profiles(cx, duplicate_active);
                            })),
                    )
                    .child(
                        Button::new("rename")
                            .small()
                            .outline()
                            .icon(IconName::Pencil)
                            .label("Renommer le profil actif")
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.edit_profiles(cx, rename_active);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(reset_profile)
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Tous les ventilateurs du profil actif repassent en Auto (BIOS)."),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("export")
                            .small()
                            .outline()
                            .icon(IconName::Download)
                            .label("Exporter (.toml)…")
                            .on_click(cx.listener(|app, _, _, cx| app.export_config(cx))),
                    )
                    .child(
                        Button::new("import")
                            .small()
                            .outline()
                            .icon(IconName::FolderOpen)
                            .label("Importer (.toml)…")
                            .on_click(cx.listener(|app, _, _, cx| app.import_config(cx))),
                    ),
            );

        let failsafe = &draft.failsafe;
        let safety = section("Sécurité (failsafe)", cx)
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Au-delà de ces seuils, tous les ventilateurs pilotés passent à 100 %.",
            ))
            .child(field(
                "Seuil critique CPU",
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
                "Seuil critique GPU",
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

        let polling = section("Acquisition", cx).child(field(
            "Intervalle de mesure",
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
        ));

        v_flex()
            .gap_4()
            .max_w(px(760.))
            .child(profiles)
            .child(safety)
            .child(polling)
            .child(app_prefs)
            .into_any_element()
    }

    /// Préférences de l'application (fichier `ui.toml`, indépendant du daemon).
    fn app_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let prefs = &self.prefs;
        let theme = cx.theme().clone();
        let toggle = |id: &'static str, checked: bool, apply: fn(&mut Prefs, bool), cx: &mut Context<Self>| {
            Switch::new(id)
                .checked(checked)
                .on_click(cx.listener(move |app, checked: &bool, _, cx| {
                    let checked = *checked;
                    app.edit_prefs(cx, |p| apply(p, checked));
                }))
        };
        let unit = prefs.temperature_unit;
        let units = h_flex()
            .gap_2()
            .child(
                choice("unit-c", "°C", unit == TempUnit::Celsius).on_click(cx.listener(
                    |app, _, _, cx| app.edit_prefs(cx, |p| p.temperature_unit = TempUnit::Celsius),
                )),
            )
            .child(
                choice("unit-f", "°F", unit == TempUnit::Fahrenheit).on_click(cx.listener(
                    |app, _, _, cx| {
                        app.edit_prefs(cx, |p| p.temperature_unit = TempUnit::Fahrenheit);
                    },
                )),
            );
        let hotkeys_label: SharedString = if self.hotkeys.is_some() {
            "Raccourcis globaux Ctrl+Shift+F1…F4 (profils 1 à 4)".into()
        } else {
            "Raccourcis globaux (indisponibles sur ce système)".into()
        };
        section("Application", cx)
            .child(field(
                "Lancer ControlAnything à l'ouverture de session",
                toggle("launch-at-startup", prefs.launch_at_startup, |p, v| p.launch_at_startup = v, cx),
                cx,
            ))
            .child(field(
                "Au démarrage de session, rester dans la zone de notification",
                toggle("start-minimized", prefs.start_minimized, |p, v| p.start_minimized = v, cx),
                cx,
            ))
            .child(field(
                "Fermer la fenêtre réduit dans la zone de notification",
                toggle("close-to-tray", prefs.close_to_tray, |p, v| p.close_to_tray = v, cx),
                cx,
            ))
            .child(field(
                hotkeys_label,
                toggle("hotkeys", prefs.hotkeys, |p, v| p.hotkeys = v, cx),
                cx,
            ))
            .child(field("Unité de température", units, cx))
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Quitter ControlAnything ne coupe pas la régulation : le service controlanythingd continue de piloter les ventilateurs.",
            ))
            .into_any_element()
    }
}
