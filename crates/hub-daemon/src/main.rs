//! controlanythingd (daemon ControlAnything) : lit les capteurs, pilote les ventilos selon le profil actif et dialogue avec l'UI.
//!
//! Sécurité : à l'arrêt propre (Ctrl+C, fermeture, extinction, SIGTERM) ou en cas de panique,
//! les contrôleurs sont lâchés et rendent les ventilos au BIOS. Un kill brutal ne peut rien
//! restaurer : le BIOS reprend la main au prochain redémarrage.

mod config_store;
mod control;
mod ipc_server;
mod providers;
mod rgb;
#[cfg(windows)]
mod service;

use control::{Command, Hub};
use rgb::RgbCmd;
use std::process::ExitCode;
use std::sync::mpsc::Sender;
use tokio::sync::{broadcast, watch};
use tracing_subscriber::fmt::MakeWriter;

/// Sous-commande demandée sur la ligne de commande.
#[derive(Debug, PartialEq)]
enum Mode {
    Console,
    Service,
    Install,
    Uninstall,
}

fn parse_mode(arg: Option<&str>) -> Result<Mode, String> {
    match arg {
        None | Some("run") => Ok(Mode::Console),
        Some("service") => Ok(Mode::Service),
        Some("install") => Ok(Mode::Install),
        Some("uninstall") => Ok(Mode::Uninstall),
        Some(other) => Err(format!(
            "argument inconnu « {other} » (attendu : run, install, uninstall, service)"
        )),
    }
}

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let result = match parse_mode(arg.as_deref()) {
        Ok(Mode::Console) => run_console(),
        #[cfg(windows)]
        Ok(Mode::Service) => service::run_dispatcher(),
        #[cfg(windows)]
        Ok(Mode::Install) => service::install(),
        #[cfg(windows)]
        Ok(Mode::Uninstall) => service::uninstall(),
        #[cfg(not(windows))]
        Ok(_) => Err(anyhow::anyhow!(
            "sous-commande réservée à Windows ; sous Linux, utilisez systemd (packaging/linux)"
        )),
        Err(message) => Err(anyhow::anyhow!(message)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("erreur : {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Logs vers `writer` + journalisation des paniques. La console PowerShell 5.1 n'interprète pas
/// les couleurs ANSI, et les fichiers n'en veulent pas.
fn init_tracing<W>(writer: W)
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(!cfg!(windows))
        .with_writer(writer)
        .init();
    // Le déroulement (unwind) qui suit lâche les contrôleurs -> ventilos rendus au BIOS.
    std::panic::set_hook(Box::new(|info| tracing::error!("panique : {info}")));
}

/// Mode console : tourne jusqu'à Ctrl+C / fermeture / extinction / SIGTERM.
fn run_console() -> anyhow::Result<()> {
    init_tracing(std::io::stdout);
    tokio::runtime::Runtime::new()?.block_on(async {
        let daemon = start()?;
        run(daemon, async {
            match shutdown_signal().await {
                Ok(signal) => tracing::info!("{signal} reçu, arrêt"),
                Err(err) => tracing::error!("signaux : {err}"),
            }
        })
        .await
    })
}

/// Daemon démarré : threads de contrôle et RGB, leurs canaux de commandes et les canaux IPC.
struct Daemon {
    commands: Sender<Command>,
    control: std::thread::JoinHandle<()>,
    rgb_commands: Sender<RgbCmd>,
    rgb: std::thread::JoinHandle<()>,
    channels: ipc_server::Channels,
}

/// Démarrage commun console/service : config, ouverture des providers, threads de contrôle et RGB.
fn start() -> anyhow::Result<Daemon> {
    let path = config_store::config_path();
    config_store::migrate_legacy(&path);
    let config = config_store::load(&path);
    tracing::info!(
        "config {} (profil {})",
        path.display(),
        config.active_profile
    );
    let (sensors, fans) = providers::build_all();

    let (live, _) = broadcast::channel(64);
    let (state_tx, state_rx) = watch::channel(String::new());
    let (rgb_state_tx, rgb_state_rx) = watch::channel(String::new());
    let (rgb_commands, rgb_rx) = std::sync::mpsc::channel();
    let rgb_config = config.rgb.clone();
    let rgb_live = live.clone();
    // Thread séparé : un appareil RGB lent ou en panique ne touche pas à la régulation des ventilos.
    let rgb = std::thread::Builder::new()
        .name("rgb".into())
        .spawn(move || rgb::run(rgb_config, rgb_rx, rgb_live, rgb_state_tx))?;
    let (commands, commands_rx) = std::sync::mpsc::channel();
    let hub = Hub::new(
        path,
        config,
        sensors,
        fans,
        live.clone(),
        state_tx,
        rgb_commands.clone(),
    );
    let control = std::thread::Builder::new()
        .name("control".into())
        .spawn(move || hub.run(commands_rx))?;
    let channels = ipc_server::Channels {
        commands: commands.clone(),
        live,
        state: state_rx,
        rgb: rgb_state_rx,
    };
    Ok(Daemon {
        commands,
        control,
        rgb_commands,
        rgb,
        channels,
    })
}

/// Sert l'IPC jusqu'à `stop`, puis arrête proprement (ventilos rendus au BIOS).
async fn run(daemon: Daemon, stop: impl Future<Output = ()>) -> anyhow::Result<()> {
    let Daemon {
        commands,
        control,
        rgb_commands,
        rgb,
        channels,
    } = daemon;
    let mut control_done = tokio::task::spawn_blocking(move || control.join());
    tokio::select! {
        result = ipc_server::serve(channels) => {
            if let Err(err) = result {
                tracing::error!("serveur IPC : {err:#}");
            }
        }
        () = stop => {}
        _ = &mut control_done => anyhow::bail!("boucle de contrôle arrêtée inopinément"),
    }
    stop_rgb(&rgb_commands, rgb).await;
    shutdown(&commands, control_done).await
}

/// Arrête le thread RGB et attend que l'éclairage soit rendu au firmware.
/// Une panique RGB est seulement journalisée : elle ne doit pas faire échouer l'arrêt des ventilos.
async fn stop_rgb(commands: &Sender<RgbCmd>, rgb: std::thread::JoinHandle<()>) {
    let _ = commands.send(RgbCmd::Stop); // déjà arrêté : rien à faire
    match tokio::task::spawn_blocking(move || rgb.join()).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => tracing::error!("le thread RGB a paniqué (ventilos non concernés)"),
        Err(err) => tracing::error!("attente du thread RGB : {err}"),
    }
}

/// Envoie `Stop` au thread de contrôle et attend qu'il ait rendu les ventilos au BIOS.
async fn shutdown(
    commands: &Sender<Command>,
    control_done: tokio::task::JoinHandle<std::thread::Result<()>>,
) -> anyhow::Result<()> {
    let _ = commands.send(Command::Stop); // déjà arrêté : rien à faire
    match control_done.await {
        Ok(Ok(())) => Ok(()),
        _ => anyhow::bail!("arrêt de la boucle de contrôle en erreur"),
    }
}

/// Attend un signal d'arrêt. Windows : fermeture console et extinction laissent ~5 s pour restaurer.
#[cfg(windows)]
async fn shutdown_signal() -> std::io::Result<&'static str> {
    use tokio::signal::windows::{ctrl_close, ctrl_shutdown};
    let mut close = ctrl_close()?;
    let mut shutdown = ctrl_shutdown()?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.map(|()| "Ctrl+C"),
        _ = close.recv() => Ok("fermeture"),
        _ = shutdown.recv() => Ok("extinction"),
    }
}

#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<&'static str> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.map(|()| "Ctrl+C"),
        _ = terminate.recv() => Ok("SIGTERM"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, parse_mode};

    #[test]
    fn parse_mode_maps_subcommands() {
        assert_eq!(parse_mode(None), Ok(Mode::Console));
        assert_eq!(parse_mode(Some("run")), Ok(Mode::Console));
        assert_eq!(parse_mode(Some("service")), Ok(Mode::Service));
        assert_eq!(parse_mode(Some("install")), Ok(Mode::Install));
        assert_eq!(parse_mode(Some("uninstall")), Ok(Mode::Uninstall));
        assert!(parse_mode(Some("--help")).is_err());
    }
}
