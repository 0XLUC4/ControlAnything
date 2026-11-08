//! Interface GPUI (via gpui-kit) de ControlAnything : tableau de bord, ventilateurs, profils,
//! zone de notification, raccourcis globaux et alertes.

mod alerts;
mod app;
mod cards;
mod chart;
mod curve_editor;
mod dashboard;
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

/// Lance l'interface et bloque jusqu'à « Quitter ».
/// `rx` reçoit les messages du daemon, `tx` lui envoie les commandes de l'UI.
/// `minimized` (`--minimized`, démarrage de session) : démarrer dans la zone de notification.
pub fn run(rx: Receiver<DaemonMsg>, tx: Sender<ClientMsg>, minimized: bool) {
    // L'app survit à la fermeture de la fenêtre (zone de notification) : on quitte explicitement.
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
                tracing::warn!("préférences non enregistrées : {err}");
            }
        }
        let show = !(minimized && prefs.start_minimized);
        let (commands_tx, commands_rx) = async_channel::unbounded();
        let tray = Tray::new(commands_tx.clone())
            .inspect_err(|err| tracing::warn!("zone de notification indisponible : {err}"))
            .ok();
        let hotkeys = Hotkeys::new(commands_tx)
            .inspect_err(|err| tracing::warn!("raccourcis globaux indisponibles : {err}"))
            .ok();
        let has_tray = tray.is_some();
        let hub = cx.new(|cx| HubApp::new(rx, tx, commands_rx, prefs, tray, hotkeys, cx));

        // L'entité HubApp (état, historique) reste vivante entre deux ouvertures de fenêtre.
        let kept = hub.clone();
        cx.on_window_closed(move |cx, _| {
            if kept.update(cx, |app, _| app.window_closed()) {
                cx.quit();
            }
        })
        .detach();

        // Sans zone de notification, une app sans fenêtre serait inatteignable.
        if show || !has_tray {
            show_window(&hub, cx);
        }
    });
}

/// Ramène la fenêtre au premier plan, ou en ouvre une nouvelle sur le même état.
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
            tracing::error!("ouverture de la fenêtre impossible : {err:#}");
            if !hub.read(cx).has_tray() {
                cx.quit();
            }
        }
    }
}
