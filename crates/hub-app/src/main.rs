//! ControlAnything: main window. Talks to the `controlanythingd` daemon (one JSON line per message).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use hub_core::ipc::{self, ClientMsg, DaemonMsg};

mod single_instance;

const RETRY_DELAY: Duration = Duration::from_secs(2);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        // The PowerShell 5.1 console does not interpret ANSI colors.
        .with_ansi(!cfg!(windows))
        .init();

    // Kept until the end of `main`: releasing it would allow a second instance.
    let Some(_instance_lock) = single_instance::acquire() else {
        tracing::info!("ControlAnything is already open: exiting");
        return;
    };

    let (daemon_tx, daemon_rx) = async_channel::bounded(16);
    let (client_tx, client_rx) = async_channel::unbounded();
    thread::spawn(move || ipc_client(daemon_tx, client_rx));
    // `--minimized`: launched at login, start in the notification area.
    let minimized = std::env::args().any(|arg| arg == "--minimized");
    hub_ui::run(daemon_rx, client_tx, minimized);
}

/// Writer of the current connection (`None` when the daemon is absent).
type SharedWriter = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

/// Daemon connection loop: reconnects every 2 s, stops when the UI is closed.
fn ipc_client(to_ui: Sender<DaemonMsg>, from_ui: Receiver<ClientMsg>) {
    let writer: SharedWriter = Arc::default();
    let shared = writer.clone();
    thread::spawn(move || write_loop(&shared, &from_ui));
    while !to_ui.is_closed() {
        let result = connect().and_then(|(reader, stream_writer)| {
            set_writer(&writer, Some(Box::new(stream_writer)));
            forward(reader, &to_ui)
        });
        set_writer(&writer, None);
        match result {
            Ok(Closed::Ui) => return,
            Ok(Closed::Daemon) => tracing::info!("daemon disconnected"),
            Err(err) => tracing::debug!("cannot connect to daemon: {err}"),
        }
        thread::sleep(RETRY_DELAY);
    }
}

fn set_writer(writer: &SharedWriter, value: Option<Box<dyn Write + Send>>) {
    // A poisoned lock only holds a stream: recover it as is.
    *writer.lock().unwrap_or_else(PoisonError::into_inner) = value;
}

enum Closed {
    Ui,
    Daemon,
}

/// Sends each UI command to the connected daemon; without a daemon, the command is dropped.
fn write_loop(writer: &SharedWriter, commands: &Receiver<ClientMsg>) {
    while let Ok(msg) = commands.recv_blocking() {
        let mut guard = writer.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(stream) = guard.as_mut() else {
            tracing::warn!("command ignored: daemon disconnected");
            continue;
        };
        let sent = ipc::encode(&msg)
            .map_err(io::Error::other)
            .and_then(|line| stream.write_all(line.as_bytes()))
            .and_then(|()| stream.flush());
        if let Err(err) = sent {
            tracing::warn!("cannot send to daemon: {err}");
            *guard = None;
        }
    }
}

/// Relays each JSON line to the UI until the end of the stream.
fn forward(stream: impl Read, to_ui: &Sender<DaemonMsg>) -> io::Result<Closed> {
    for line in BufReader::new(stream).lines() {
        match ipc::decode(&line?) {
            Ok(msg) => {
                if to_ui.send_blocking(msg).is_err() {
                    return Ok(Closed::Ui);
                }
            }
            Err(err) => tracing::warn!("invalid message ignored: {err}"),
        }
    }
    Ok(Closed::Daemon)
}

#[cfg(windows)]
fn connect() -> io::Result<(std::fs::File, std::fs::File)> {
    let pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(ipc::PIPE_NAME)?;
    let writer = pipe.try_clone()?;
    Ok((pipe, writer))
}

#[cfg(unix)]
fn connect() -> io::Result<(
    std::os::unix::net::UnixStream,
    std::os::unix::net::UnixStream,
)> {
    let stream = std::os::unix::net::UnixStream::connect(ipc::SOCKET_PATH)?;
    let writer = stream.try_clone()?;
    Ok((stream, writer))
}
