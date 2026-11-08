//! ControlAnything GPUI interface (via gpui-kit): dashboard, fans, profiles,
//! system tray, global hotkeys and alerts.

mod alerts;
mod app;
mod cards;
mod chart;
mod curve_editor;
mod dashboard;
mod devices;
mod fans;
mod hotkeys;
mod icons;
mod lighting;
mod prefs;
mod settings;
mod state;
mod tray;
mod widgets;

use async_channel::{Receiver, Sender};
use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::{App, AppContext as _, Entity, QuitMode, WindowBounds, WindowOptions, px, size};
use hub_core::ipc::{ClientMsg, DaemonMsg};

use app::HubApp;
use hotkeys::Hotkeys;
use prefs::Prefs;
use tray::Tray;

/// Starts the interface and blocks until "Quit".
/// `rx` receives daemon messages, `tx` sends it the UI commands.
/// `minimized` (`--minimized`, session startup): start in the system tray.
pub fn run(rx: Receiver<DaemonMsg>, tx: Sender<ClientMsg>, minimized: bool) {
    // The app outlives its window (system tray): quitting is explicit.
    let app = gpui_kit::application()
        .with_assets(icons::HubAssets)
        .with_quit_mode(QuitMode::Explicit);
    app.run(move |cx: &mut App| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        widgets::apply_theme(cx);

        let mut prefs = Prefs::load();
        if prefs::migrate_legacy_autostart() && !prefs.launch_at_startup {
            prefs.launch_at_startup = true;
            if let Err(err) = prefs.save() {
                tracing::warn!("preferences not saved: {err}");
            }
        }
        let show = !(minimized && prefs.start_minimized);
        let (commands_tx, commands_rx) = async_channel::unbounded();
        let tray = Tray::new(commands_tx.clone())
            .inspect_err(|err| tracing::warn!("system tray unavailable: {err}"))
            .ok();
        let hotkeys = Hotkeys::new(commands_tx)
            .inspect_err(|err| tracing::warn!("global hotkeys unavailable: {err}"))
            .ok();
        let has_tray = tray.is_some();
        let hub = cx.new(|cx| HubApp::new(rx, tx, commands_rx, prefs, tray, hotkeys, cx));

        // The HubApp entity (state, history) stays alive between two window openings.
        let kept = hub.clone();
        cx.on_window_closed(move |cx, _| {
            if kept.update(cx, |app, _| app.window_closed()) {
                cx.quit();
            }
        })
        .detach();

        // Without a system tray, a windowless app would be unreachable.
        if show || !has_tray {
            show_window(&hub, cx);
        }
    });
}

/// Brings the window to the front, or opens a new one on the same state.
pub fn show_window(hub: &Entity<HubApp>, cx: &mut App) {
    if let Some(handle) = hub.read(cx).window
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1240.), px(820.)), cx)),
        window_min_size: Some(size(px(960.), px(640.))),
        ..TitleBar::window_options()
    };
    let view = hub.clone();
    let opened = gpui_kit::open_window(options, cx, move |window, cx| {
        view.update(cx, |app, cx| app.attach_window(window, cx));
        view
    });
    match opened {
        Ok((handle, _)) => hub.update(cx, |app, _| app.window = Some(handle)),
        Err(err) => {
            tracing::error!("cannot open window: {err:#}");
            if !hub.read(cx).has_tray() {
                cx.quit();
            }
        }
    }
}
