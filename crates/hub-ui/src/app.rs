//! Vue racine : barre de titre, navigation latérale, réception des messages du daemon.
//! L'entité vit au niveau de l'application : elle survit à la fermeture de la fenêtre
//! (zone de notification) ; seuls les contrôles à état sont recréés à chaque ouverture.

use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::InputState;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::sidebar::{Sidebar, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, TitleBar, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyWindowHandle, AppContext as _, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
    px,
};
use hub_core::ipc::{ClientMsg, DaemonMsg};

use crate::alerts::{Alerts, notify};
use crate::dashboard::Dashboard;
use crate::fans::{FanControls, FansPage};
use crate::hotkeys::{Hotkeys, profile_for_slot};
use crate::prefs::Prefs;
use crate::state::{HubState, Notice};
use crate::tray::{Command, Tray};
use crate::widgets::{ACCENT_HOVER, GREEN, RED, RADIUS, hex, solid_tag};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dashboard,
    Fans,
    Settings,
}

/// Contrôles à état liés à la fenêtre ouverte (recréés à chaque ouverture).
#[derive(Clone)]
pub struct Controls {
    profile_select: Entity<SelectState<Vec<SharedString>>>,
    pub fans: FanControls,
    pub profile_name: Entity<InputState>,
}

pub struct HubApp {
    pub state: HubState,
    pub prefs: Prefs,
    page: Page,
    dashboard: Dashboard,
    pub fans: FansPage,
    /// `None` quand l'application est réduite dans la zone de notification.
    pub controls: Option<Controls>,
    pub window: Option<AnyWindowHandle>,
    tray: Option<Tray>,
    pub hotkeys: Option<Hotkeys>,
    alerts: Alerts,
    /// Premier clic sur « Réinitialiser le profil » : confirmation attendue jusqu'à cet instant.
    pub confirm_reset_until: Option<Instant>,
    tx: Sender<ClientMsg>,
    window_subscriptions: Vec<Subscription>,
}

impl HubApp {
    pub fn new(
        rx: Receiver<DaemonMsg>,
        tx: Sender<ClientMsg>,
        commands: Receiver<Command>,
        prefs: Prefs,
        tray: Option<Tray>,
        mut hotkeys: Option<Hotkeys>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Réception des messages sur le thread UI ; s'arrête si l'entité est détruite.
        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                if this.update(cx, |app, cx| app.receive(msg, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Commandes de la zone de notification et des raccourcis globaux.
        cx.spawn(async move |this, cx| {
            while let Ok(command) = commands.recv().await {
                if this.update(cx, |app, cx| app.command(command, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Chaque seconde : statut « déconnecté » sans nouveau message, alerte daemon arrêté.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let ticked = this.update(cx, |app, cx| {
                    if app.alerts.daemon_lost(app.state.last_update(), Instant::now()) {
                        notify("ControlAnything : service arrêté", "Ventilateurs rendus au BIOS.");
                    }
                    cx.notify();
                });
                if ticked.is_err() {
                    break;
                }
            }
        })
        .detach();

        if let Some(hotkeys) = hotkeys.as_mut() {
            hotkeys.set_enabled(prefs.hotkeys);
        }
        Self {
            state: HubState::default(),
            prefs,
            page: Page::Dashboard,
            dashboard: Dashboard::default(),
            fans: FansPage::default(),
            controls: None,
            window: None,
            tray,
            hotkeys,
            alerts: Alerts::default(),
            confirm_reset_until: None,
            tx,
            window_subscriptions: Vec::new(),
        }
    }

    /// Crée les contrôles à état de la nouvelle fenêtre.
    pub fn attach_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let profile_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let mut subscriptions = vec![cx.subscribe_in(
            &profile_select,
            window,
            |app: &mut Self, _, event: &SelectEvent<Vec<SharedString>>, _, _| {
                if let SelectEvent::Confirm(Some(name)) = event {
                    app.activate_profile(name.to_string());
                }
            },
        )];
        let (fans, fan_subscriptions) = FanControls::new(window, cx);
        subscriptions.extend(fan_subscriptions);
        let profile_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Nom du nouveau profil"));
        self.fans.source_items.clear();
        self.controls = Some(Controls {
            profile_select,
            fans,
            profile_name,
        });
        self.window_subscriptions = subscriptions;
    }

    /// Fenêtre fermée : libère ses contrôles. Renvoie `true` s'il faut quitter l'application.
    pub fn window_closed(&mut self) -> bool {
        self.window = None;
        self.controls = None;
        self.window_subscriptions.clear();
        self.fans.curve.dragging = None;
        !(self.prefs.close_to_tray && self.has_tray())
    }

    pub fn has_tray(&self) -> bool {
        self.tray.is_some()
    }

    fn receive(&mut self, msg: DaemonMsg, cx: &mut Context<Self>) {
        let profiles_changed = matches!(msg, DaemonMsg::State { .. });
        if let DaemonMsg::Snapshot(snapshot) = &msg {
            self.dashboard.apply(snapshot);
        }
        self.state.receive(msg);
        if self.alerts.failsafe_rising(self.state.failsafe) {
            notify(
                "Surchauffe détectée — ventilateurs à 100 %",
                "Seuil critique CPU/GPU dépassé : tous les ventilateurs pilotés sont forcés à 100 %.",
            );
        }
        if profiles_changed {
            self.sync_tray();
        }
        if self.fans.selected.is_none() {
            self.fans.selected = self.state.channels.first().map(|c| c.id.clone());
        }
        cx.notify();
    }

    fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        match command {
            Command::Open => {
                // Hors de la mise à jour de l'entité : l'ouverture la met à jour à son tour.
                let hub = cx.entity();
                cx.defer(move |cx| crate::show_window(&hub, cx));
            }
            Command::Quit => cx.quit(),
            Command::SetProfile(name) => self.activate_profile(name),
            Command::ProfileSlot(slot) => {
                let name = self
                    .state
                    .config
                    .as_ref()
                    .and_then(|c| profile_for_slot(c, slot))
                    .map(str::to_owned);
                if let Some(name) = name {
                    self.activate_profile(name);
                }
            }
        }
        // Un clic coche l'élément du menu même si rien ne change : on resynchronise.
        self.sync_tray();
    }

    /// Demande au daemon d'activer un profil (sans effet s'il l'est déjà).
    pub fn activate_profile(&self, name: String) {
        let active = self.state.config.as_ref().map(|c| c.active_profile.as_str());
        if active != Some(name.as_str()) {
            self.send(ClientMsg::SetProfile { name });
        }
    }

    fn sync_tray(&mut self) {
        let (Some(tray), Some(config)) = (self.tray.as_mut(), self.state.config.as_ref()) else {
            return;
        };
        let names = config.profiles.iter().map(|p| p.name.clone()).collect();
        tray.set_profiles(names, config.active_profile.clone());
    }

    pub fn send(&self, msg: ClientMsg) {
        // Canal non borné : échoue seulement si le client IPC est arrêté.
        if self.tx.try_send(msg).is_err() {
            tracing::warn!("client IPC arrêté : commande perdue");
        }
    }

    /// Valide localement le brouillon puis l'envoie au daemon.
    pub fn apply_draft(&mut self, cx: &mut Context<Self>) {
        let Some(mut config) = self.state.draft.clone() else {
            return;
        };
        match config.validate() {
            Ok(()) => {
                self.state.pending_apply = true;
                self.state.notice = None;
                self.send(ClientMsg::SetConfig { config });
            }
            Err(err) => self.state.notice = Some(Notice::Error(err.to_string())),
        }
        cx.notify();
    }

    pub fn reset_draft(&mut self, cx: &mut Context<Self>) {
        self.state.draft.clone_from(&self.state.config);
        self.fans.curve.dragging = None;
        cx.notify();
    }

    pub fn notify_error(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.state.notice = Some(Notice::Error(message.into()));
        cx.notify();
    }

    /// Aligne le sélecteur de profil sur la config du daemon.
    fn sync_profile_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(config), Some(controls)) = (self.state.config.as_ref(), self.controls.as_ref())
        else {
            return;
        };
        let names: Vec<SharedString> = config
            .profiles
            .iter()
            .map(|p| SharedString::from(p.name.clone()))
            .collect();
        let active = SharedString::from(config.active_profile.clone());
        controls.profile_select.update(cx, |select, cx| {
            if select.selected_value() != Some(&active) {
                select.set_items(names, window, cx);
                select.set_selected_value(&active, window, cx);
            }
        });
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let item = |label: &'static str, icon: IconName, page: Page, cx: &mut Context<Self>| {
            SidebarMenuItem::new(label)
                .icon(icon)
                .px_3()
                .py_2p5()
                .active(self.page == page)
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.page = page;
                    cx.notify();
                }))
        };
        Sidebar::new("nav").w(px(220.)).child(
            SidebarMenu::new()
                .gap_3()
                .pt_3()
                .child(item("Tableau de bord", IconName::LayoutDashboard, Page::Dashboard, cx))
                .child(item("Ventilateurs", IconName::Fan, Page::Fans, cx))
                .child(item("Paramètres", IconName::Settings, Page::Settings, cx)),
        )
    }

    fn title_bar(&self) -> impl IntoElement {
        let status = if self.state.connected() {
            solid_tag(GREEN).rounded_full().child("Connecté")
        } else {
            solid_tag(RED).rounded_full().child("Daemon déconnecté")
        };
        let profile_select = self
            .controls
            .as_ref()
            .filter(|_| self.state.config.is_some())
            .map(|c| c.profile_select.clone());
        TitleBar::new().child(
            h_flex()
                .flex_1()
                .justify_between()
                .pr_3()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .text_sm()
                        .font_semibold()
                        .child(Icon::new(IconName::Fan).small().text_color(hex(ACCENT_HOVER)))
                        .child("ControlAnything"),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .when(self.state.failsafe, |row| {
                            row.child(
                                solid_tag(RED)
                                    .rounded_full()
                                    .child("SURCHAUFFE – ventilateurs à 100 %"),
                            )
                        })
                        .when_some(profile_select, |row, select| {
                            // Toute la barre est une zone « déplacer la fenêtre » (HTCAPTION sous
                            // Windows) : sans `occlude`, Windows avale le clic avant GPUI.
                            row.child(
                                div().occlude().child(
                                    Select::new(&select)
                                        .id("profile")
                                        .small()
                                        .title_prefix("Profil : ")
                                        .w(px(220.)),
                                ),
                            )
                        })
                        .child(status),
                ),
        )
    }

    /// Bandeau d'information ou d'erreur, fermable.
    fn notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let (text, color) = match self.state.notice.as_ref()? {
            Notice::Info(text) => (text.clone(), theme.success),
            Notice::Error(text) => (text.clone(), theme.danger),
        };
        Some(
            h_flex()
                .justify_between()
                .gap_3()
                .p_3()
                .rounded(px(RADIUS))
                .bg(color.opacity(0.15))
                .text_color(color)
                .child(div().flex_1().text_sm().child(text))
                .child(
                    Button::new("close-notice")
                        .ghost()
                        .small()
                        .icon(IconName::X)
                        .on_click(cx.listener(|app, _, _, cx| {
                            app.state.notice = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// Barre « Appliquer / Annuler » tant que le brouillon diffère de la config du daemon.
    fn apply_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.state.dirty() || self.page == Page::Dashboard {
            return None;
        }
        let theme = cx.theme();
        Some(
            h_flex()
                .justify_between()
                .p_3()
                .rounded(px(RADIUS))
                .border_1()
                .border_color(theme.primary.opacity(0.5))
                .bg(theme.primary.opacity(0.1))
                .child(div().text_sm().child("Modifications non appliquées"))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("cancel-draft")
                                .outline()
                                .small()
                                .icon(IconName::Undo2)
                                .label("Annuler")
                                .on_click(cx.listener(|app, _, _, cx| app.reset_draft(cx))),
                        )
                        .child(
                            Button::new("apply-draft")
                                .primary()
                                .small()
                                .icon(IconName::Check)
                                .label("Appliquer")
                                .on_click(cx.listener(|app, _, _, cx| app.apply_draft(cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl Render for HubApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_profile_select(window, cx);
        let page = match self.page {
            Page::Dashboard => self.dashboard.render(&self.state, self.prefs.temperature_unit, cx),
            Page::Fans => self.render_fans(window, cx),
            Page::Settings => self.render_settings(cx),
        };
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.title_bar())
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(self.sidebar(cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .p_4()
                            .gap_4()
                            .children(self.notice(cx))
                            .children(self.apply_bar(cx))
                            .child(page)
                            .overflow_y_scrollbar(),
                    ),
            )
    }
}
