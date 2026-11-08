//! ControlAnything : fenêtre principale. Dialogue avec le daemon `controlanythingd` (une ligne JSON par message).
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
        // La console PowerShell 5.1 n'interprète pas les couleurs ANSI.
        .with_ansi(!cfg!(windows))
        .init();

    // Gardé jusqu'à la fin de `main` : le relâcher autoriserait une seconde instance.
    let Some(_instance_lock) = single_instance::acquire() else {
        tracing::info!("ControlAnything est déjà ouvert : sortie");
        return;
    };

    let (daemon_tx, daemon_rx) = async_channel::bounded(16);
    let (client_tx, client_rx) = async_channel::unbounded();
    thread::spawn(move || ipc_client(daemon_tx, client_rx));
    // `--minimized` : lancé à l'ouverture de session, démarrer dans la zone de notification.
    let minimized = std::env::args().any(|arg| arg == "--minimized");
    hub_ui::run(daemon_rx, client_tx, minimized);
}

/// Écrivain de la connexion courante (`None` quand le daemon est absent).
type SharedWriter = Arc<Mutex<Option<Box<dyn Write + Send>>>>;

/// Boucle de connexion au daemon : reconnecte toutes les 2 s, s'arrête si l'UI est fermée.
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
            Ok(Closed::Daemon) => tracing::info!("daemon déconnecté"),
            Err(err) => tracing::debug!("connexion au daemon impossible : {err}"),
        }
        thread::sleep(RETRY_DELAY);
    }
}

fn set_writer(writer: &SharedWriter, value: Option<Box<dyn Write + Send>>) {
    // Un verrou empoisonné ne contient qu'un flux : on le récupère tel quel.
    *writer.lock().unwrap_or_else(PoisonError::into_inner) = value;
}

enum Closed {
    Ui,
    Daemon,
}

/// Envoie chaque commande de l'UI au daemon connecté ; sans daemon, la commande est perdue.
fn write_loop(writer: &SharedWriter, commands: &Receiver<ClientMsg>) {
    while let Ok(msg) = commands.recv_blocking() {
        let mut guard = writer.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(stream) = guard.as_mut() else {
            tracing::warn!("commande ignorée : daemon déconnecté");
            continue;
        };
        let sent = ipc::encode(&msg)
            .map_err(io::Error::other)
            .and_then(|line| stream.write_all(line.as_bytes()))
            .and_then(|()| stream.flush());
        if let Err(err) = sent {
            tracing::warn!("envoi au daemon impossible : {err}");
            *guard = None;
        }
    }
}

/// Relaie chaque ligne JSON vers l'UI jusqu'à la fin du flux.
fn forward(stream: impl Read, to_ui: &Sender<DaemonMsg>) -> io::Result<Closed> {
    for line in BufReader::new(stream).lines() {
        match ipc::decode(&line?) {
            Ok(msg) => {
                if to_ui.send_blocking(msg).is_err() {
                    return Ok(Closed::Ui);
                }
            }
            Err(err) => tracing::warn!("message invalide ignoré : {err}"),
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
