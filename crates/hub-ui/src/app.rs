//! Root view: title bar, side navigation, reception of daemon messages.
//! The entity lives at application level: it outlives the window
//! (system tray); only the stateful controls are recreated on each opening.

use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use gpui_kit::assets::IconName;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::InputState;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, TitleBar, h_flex,
    tooltip::Tooltip, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, AnyWindowHandle, AppContext as _, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div,
    img, px,
};
use hub_core::ipc::{ClientMsg, DaemonMsg};
use hub_core::{Device, SensorKind};

use crate::alerts::{Alerts, notify};
use crate::dashboard::{Dashboard, find_kind};
use crate::fans::{FanControls, FansPage};
use crate::hotkeys::{Hotkeys, profile_for_slot};
use crate::icons::BRAND_ICON;
use crate::lighting::{LightingControls, LightingPage};
use crate::prefs::Prefs;
use crate::state::{HubState, Notice};
use crate::tray::{Command, Tray};
use crate::updater::{self, UpdateState};
use crate::widgets::{GREEN, RADIUS, RED, solid_tag};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dashboard,
    Fans,
    Lighting,
    Devices,
    Audio,
    Network,
    Automations,
    Settings,
}

/// Stateful controls tied to the open window (recreated on each opening).
#[derive(Clone)]
pub struct Controls {
    profile_select: Entity<SelectState<Vec<SharedString>>>,
    pub fans: FanControls,
    pub lighting: LightingControls,
    pub profile_name: Entity<InputState>,
}

pub struct HubApp {
    pub state: HubState,
    pub prefs: Prefs,
    pub page: Page,
    pub(crate) dashboard: Dashboard,
    pub fans: FansPage,
    pub lighting: LightingPage,
    /// RGB device shown in detail on the Devices page (`None`: the list).
    pub device_detail: Option<String>,
    /// Owned by `settings.rs`.
    pub settings: crate::settings::SettingsPage,
    /// Owned by `updater.rs`.
    pub update: UpdateState,
    /// Owned by `automations.rs`.
    pub automations: crate::automations::AutomationsPage,
    /// Audio and Network pages, owned by `system/`.
    pub system: crate::system::SystemPages,
    /// `None` when the application is minimized to the system tray.
    pub controls: Option<Controls>,
    pub window: Option<AnyWindowHandle>,
    tray: Option<Tray>,
    pub hotkeys: Option<Hotkeys>,
    alerts: Alerts,
    /// First click on "Reset profile": confirmation expected until this instant.
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
        // Receives messages on the UI thread; stops if the entity is dropped.
        cx.spawn(async move |this, cx| {
            while let Ok(msg) = rx.recv().await {
                if this.update(cx, |app, cx| app.receive(msg, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Commands from the system tray and global hotkeys.
        cx.spawn(async move |this, cx| {
            while let Ok(command) = commands.recv().await {
                if this.update(cx, |app, cx| app.command(command, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        // Every second: "disconnected" status without new messages, daemon stopped alert.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let ticked = this.update(cx, |app, cx| {
                    if app
                        .alerts
                        .daemon_lost(app.state.last_update(), Instant::now())
                    {
                        notify(
                            "ControlAnything: service stopped",
                            "Fans handed back to the BIOS.",
                        );
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
            lighting: LightingPage::default(),
            device_detail: None,
            settings: Default::default(),
            update: UpdateState::start(),
            automations: Default::default(),
            system: Default::default(),
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

    /// Creates the stateful controls of the new window.
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
        let (lighting, lighting_subscriptions) = LightingControls::new(window, cx);
        subscriptions.extend(lighting_subscriptions);
        let profile_name = cx.new(|cx| InputState::new(window, cx).placeholder("New profile name"));
        self.fans.source_items.clear();
        self.controls = Some(Controls {
            profile_select,
            fans,
            lighting,
            profile_name,
        });
        self.window_subscriptions = subscriptions;
    }

    /// Window closed: frees its controls. Returns `true` if the application must quit.
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
        if let DaemonMsg::State { auto_profile, .. } = &msg {
            self.automations.active.clone_from(auto_profile);
        }
        if let DaemonMsg::Snapshot(snapshot) = &msg {
            self.dashboard.apply(snapshot);
            let fired =
                self.alerts
                    .check_thresholds(&snapshot.readings, &self.prefs, Instant::now());
            for (summary, body) in fired {
                notify(summary, body);
            }
            if let Some(tray) = self.tray.as_mut() {
                tray.set_cpu_temp(
                    find_kind(&snapshot.readings, Device::Cpu, SensorKind::Temperature)
                        .map(|r| self.prefs.temperature_unit.format(r.value, 0)),
                );
            }
        }
        self.state.receive(msg);
        if self.alerts.failsafe_rising(self.state.failsafe) {
            notify(
                "Overheating: fans at 100%",
                "Critical CPU/GPU threshold exceeded: all controlled fans are forced to 100%.",
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
                // Outside the entity update: opening updates it in turn.
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
        // A click checks the menu item even if nothing changes: resync.
        self.sync_tray();
    }

    /// Asks the daemon to activate a profile (no effect if already active).
    pub fn activate_profile(&self, name: String) {
        let active = self
            .state
            .config
            .as_ref()
            .map(|c| c.active_profile.as_str());
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
        // Unbounded channel: fails only if the IPC client is stopped.
        if self.tx.try_send(msg).is_err() {
            tracing::warn!("IPC client stopped: command lost");
        }
    }

    /// Validates the draft locally then sends it to the daemon.
    pub fn apply_draft(&mut self, cx: &mut Context<Self>) {
        let Some(mut config) = self.state.draft.clone() else {
            return;
        };
        match config.validate() {
            Ok(()) => {
                self.state.start_pending();
                self.state.notice = None;
                self.send(ClientMsg::SetConfig { config });
            }
            Err(err) => self.state.notice = Some(Notice::Error(err.to_string())),
        }
        cx.notify();
    }

    /// A config was sent to the service and its answer is awaited (at most 5 s):
    /// buttons that wait on it show `loading` and stay disabled.
    pub fn is_pending(&self) -> bool {
        self.state.pending(Instant::now())
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

    /// Aligns the profile selector with the daemon config.
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
            // `min_h`: the item forces `h_7` after our style, too short for the highlight.
            SidebarMenuItem::new(label)
                .icon(icon)
                .min_h(px(40.))
                .px_3()
                .rounded(px(8.))
                .active(self.page == page)
                .on_click(cx.listener(move |app, _, _, cx| {
                    app.page = page;
                    app.device_detail = None;
                    cx.notify();
                }))
        };
        let connected = self.state.connected();
        let group = |label: &'static str, items: Vec<SidebarMenuItem>| {
            SidebarGroup::new(label).child(SidebarMenu::new().gap_1().children(items))
        };
        Sidebar::new("nav")
            .w(px(220.))
            .child(group(
                "Control",
                vec![
                    item("Dashboard", IconName::LayoutDashboard, Page::Dashboard, cx).suffix(
                        move |_, _| {
                            let (icon, color, tip) = if connected {
                                (IconName::Check, GREEN, "Service connected")
                            } else {
                                (IconName::X, RED, "Service offline")
                            };
                            div()
                                .id("service-status")
                                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                                .child(Icon::new(icon).small().text_color(gpui_kit::rgb(color)))
                        },
                    ),
                    item("Fans", IconName::Fan, Page::Fans, cx),
                    item("Lighting", IconName::Lightbulb, Page::Lighting, cx),
                    item("Devices", IconName::Microchip, Page::Devices, cx),
                ],
            ))
            .child(group(
                "System",
                vec![
                    item("Audio", IconName::Volume2, Page::Audio, cx),
                    item("Network", IconName::Wifi, Page::Network, cx),
                ],
            ))
            .child(group(
                "App",
                vec![
                    item("Automations", IconName::Zap, Page::Automations, cx),
                    item("Settings", IconName::Settings, Page::Settings, cx),
                ],
            ))
    }

    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let profile_select = self
            .controls
            .as_ref()
            .filter(|_| self.state.config.is_some())
            .map(|c| c.profile_select.clone());
        TitleBar::new().bg(cx.theme().background).child(
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
                        .child(img(BRAND_ICON).size(px(18.)).rounded_sm())
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
                                    .child("Overheating: fans at 100%"),
                            )
                        })
                        .when_some(profile_select, |row, select| {
                            // The whole bar is a "move window" area (HTCAPTION on
                            // Windows): without `occlude`, Windows swallows the click before GPUI.
                            row.child(
                                div().occlude().child(
                                    Select::new(&select)
                                        .id("profile")
                                        .small()
                                        .title_prefix("Profile: ")
                                        .w(px(220.)),
                                ),
                            )
                        })
                        .child(div().occlude().child(self.update_popover(cx))),
                ),
        )
    }

    /// Update icon (blue dot when a newer version is known) opening a small popup:
    /// version, release notes and "Update and restart", or "Check again" when up to date.
    fn update_popover(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let trigger = Button::new("check-update")
            .small()
            .ghost()
            .icon(IconName::CircleArrowUp)
            .tooltip("Updates")
            .loading(updater::busy(&self.update));
        let app = cx.entity();
        let popover = Popover::new("update-popover")
            .anchor(Anchor::TopRight)
            .trigger(trigger)
            .content(move |_, _, cx| {
                let (status, release, busy, progress) = {
                    let state = &app.read(cx).update;
                    (
                        updater::status(state),
                        updater::release(state),
                        updater::busy(state),
                        updater::download_progress(state),
                    )
                };
                let muted = cx.theme().muted_foreground;
                let action = app.clone();
                let body = match release {
                    Some(release) => v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_base()
                                .font_semibold()
                                .child(format!("ControlAnything {} is available", release.version)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(format!("You have {}.", updater::CURRENT)),
                        )
                        .when(!release.notes.is_empty(), |col| {
                            col.child(div().text_sm().child(release.notes))
                        })
                        .map(|col| match progress {
                            Some(percent) => col
                                .child(Progress::new("update-progress").value(percent))
                                .child(div().text_xs().text_color(muted).child("Downloading...")),
                            None => col.child(
                                Button::new("update-install")
                                    .primary()
                                    .w_full()
                                    .label("Update and restart")
                                    .disabled(busy)
                                    .on_click(move |_, _, cx| {
                                        action.update(cx, updater::install_latest);
                                    }),
                            ),
                        })
                        .child(div().text_xs().text_color(muted).child(
                            "Fans return to firmware control while the service restarts.",
                        )),
                    None => v_flex().child(
                        h_flex()
                            .justify_between()
                            .gap_4()
                            .child(
                                v_flex()
                                    .child(
                                        div()
                                            .text_base()
                                            .font_semibold()
                                            .child("You're up to date"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted)
                                            .child(format!("ControlAnything {}", updater::CURRENT)),
                                    ),
                            )
                            .child(
                                Button::new("update-refresh")
                                    .ghost()
                                    .icon(IconName::RefreshCw)
                                    .tooltip("Check again")
                                    .loading(busy)
                                    .on_click(move |_, _, cx| {
                                        action.update(cx, updater::check);
                                    }),
                            ),
                    ),
                };
                body.w(px(300.)).when_some(status, |col, status| {
                    col.child(div().text_xs().text_color(muted).child(status))
                })
            });
        // The dot sits over the icon corner and lets clicks through to the button.
        div()
            .relative()
            .child(popover)
            .when(updater::has_update(&self.update), |wrap| {
                wrap.child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .right(px(2.))
                        .size(px(7.))
                        .rounded_full()
                        .bg(cx.theme().primary),
                )
            })
    }

    /// Closable success or error banner.
    fn notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let alert = match self.state.notice.as_ref()? {
            Notice::Info(text) => Alert::success("notice", text.clone()),
            Notice::Error(text) => Alert::error("notice", text.clone()),
        };
        Some(
            alert
                .on_close(cx.listener(|app, _, _, cx| {
                    app.state.notice = None;
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// "Apply / Cancel" bar while the draft differs from the daemon config.
    fn apply_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.state.dirty() || self.page == Page::Dashboard {
            return None;
        }
        let pending = self.is_pending();
        let theme = cx.theme();
        Some(
            h_flex()
                .justify_between()
                .p_3()
                .rounded(px(RADIUS))
                .border_1()
                .border_color(theme.primary.opacity(0.5))
                .bg(theme.primary.opacity(0.1))
                .child(div().text_sm().child("Unapplied changes"))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("cancel-draft")
                                .outline()
                                .small()
                                .icon(IconName::Undo2)
                                .label("Cancel")
                                .disabled(pending)
                                .on_click(cx.listener(|app, _, _, cx| app.reset_draft(cx))),
                        )
                        .child(
                            Button::new("apply-draft")
                                .primary()
                                .small()
                                .icon(IconName::Check)
                                .label("Apply")
                                .loading(pending)
                                .disabled(pending)
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
            Page::Dashboard => self.render_dashboard(cx),
            Page::Fans => self.render_fans(window, cx),
            Page::Lighting => self.render_lighting(window, cx),
            Page::Devices => match self.device_detail.clone() {
                Some(id) => self.render_device_detail(&id, window, cx),
                None => self.render_devices(cx),
            },
            Page::Audio => self.render_audio(cx),
            Page::Network => self.render_network(cx),
            Page::Automations => self.render_automations(window, cx),
            Page::Settings => self.render_settings(cx),
        };
        let footer: Vec<AnyElement> = self
            .notice(cx)
            .into_iter()
            .chain(self.apply_bar(cx))
            .collect();
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.title_bar(cx))
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
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .p_4()
                                    .gap_4()
                                    .child(page)
                                    .overflow_y_scrollbar(),
                            )
                            // Outside the scroll column: always visible.
                            .when(!footer.is_empty(), |column| {
                                column.child(
                                    v_flex().flex_none().gap_2().px_4().pb_4().children(footer),
                                )
                            }),
                    ),
            )
    }
}
