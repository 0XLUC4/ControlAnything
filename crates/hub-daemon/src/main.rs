//! controlanythingd (ControlAnything daemon): reads the sensors, drives the fans from the active profile and talks to the UI.
//!
//! Safety: on a clean stop (Ctrl+C, close, shutdown, SIGTERM) or on panic, the controllers are
//! dropped and hand the fans back to the BIOS. A hard kill cannot restore anything: the BIOS
//! takes over again at the next reboot.

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

/// Subcommand requested on the command line.
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
            "unknown argument \"{other}\" (expected: run, install, uninstall, service)"
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
            "Windows-only subcommand; on Linux, use systemd (packaging/linux)"
        )),
        Err(message) => Err(anyhow::anyhow!(message)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Logs to `writer` + panic logging. The PowerShell 5.1 console does not interpret
/// ANSI colors, and files do not want them.
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
    // The unwind that follows drops the controllers -> fans handed back to the BIOS.
    std::panic::set_hook(Box::new(|info| tracing::error!("panic: {info}")));
}

/// Console mode: runs until Ctrl+C / close / shutdown / SIGTERM.
fn run_console() -> anyhow::Result<()> {
    init_tracing(std::io::stdout);
    tokio::runtime::Runtime::new()?.block_on(async {
        let daemon = start()?;
        run(daemon, async {
            match shutdown_signal().await {
                Ok(signal) => tracing::info!("{signal} received, stopping"),
                Err(err) => tracing::error!("signals: {err}"),
            }
        })
        .await
    })
}

/// Started daemon: control and RGB threads, their command channels and the IPC channels.
struct Daemon {
    commands: Sender<Command>,
    control: std::thread::JoinHandle<()>,
    rgb_commands: Sender<RgbCmd>,
    rgb: std::thread::JoinHandle<()>,
    channels: ipc_server::Channels,
}

/// Common console/service startup: config, opening providers, control and RGB threads.
fn start() -> anyhow::Result<Daemon> {
    let path = config_store::config_path();
    config_store::migrate_legacy(&path);
    let config = config_store::load(&path);
    tracing::info!(
        "config {} (profile {})",
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
    // Separate thread: a slow or panicking RGB device does not affect fan control.
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

/// Serves IPC until `stop`, then shuts down cleanly (fans handed back to the BIOS).
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
                tracing::error!("IPC server: {err:#}");
            }
        }
        () = stop => {}
        _ = &mut control_done => anyhow::bail!("control loop stopped unexpectedly"),
    }
    stop_rgb(&rgb_commands, rgb).await;
    shutdown(&commands, control_done).await
}

/// Stops the RGB thread and waits for the lighting to be handed back to firmware.
/// An RGB panic is only logged: it must not make the fan shutdown fail.
async fn stop_rgb(commands: &Sender<RgbCmd>, rgb: std::thread::JoinHandle<()>) {
    let _ = commands.send(RgbCmd::Stop); // already stopped: nothing to do
    match tokio::task::spawn_blocking(move || rgb.join()).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => tracing::error!("the RGB thread panicked (fans not affected)"),
        Err(err) => tracing::error!("waiting for the RGB thread: {err}"),
    }
}

/// Sends `Stop` to the control thread and waits until it has handed the fans back to the BIOS.
async fn shutdown(
    commands: &Sender<Command>,
    control_done: tokio::task::JoinHandle<std::thread::Result<()>>,
) -> anyhow::Result<()> {
    let _ = commands.send(Command::Stop); // already stopped: nothing to do
    match control_done.await {
        Ok(Ok(())) => Ok(()),
        _ => anyhow::bail!("control loop stopped with an error"),
    }
}

/// Waits for a stop signal. Windows: console close and shutdown leave ~5 s to restore.
#[cfg(windows)]
async fn shutdown_signal() -> std::io::Result<&'static str> {
    use tokio::signal::windows::{ctrl_close, ctrl_shutdown};
    let mut close = ctrl_close()?;
    let mut shutdown = ctrl_shutdown()?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.map(|()| "Ctrl+C"),
        _ = close.recv() => Ok("close"),
        _ = shutdown.recv() => Ok("shutdown"),
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
