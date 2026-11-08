//! Serveur IPC local : diffuse l'état et le flux live, relaie les commandes au thread de contrôle.

use crate::control::Command;
use hub_core::ipc::{self, ClientMsg, DaemonMsg};
use std::sync::mpsc::Sender;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, watch};

/// Taille maximale d'une commande client ; au-delà, le client est déconnecté.
const MAX_LINE: usize = 1024 * 1024;

/// Canaux partagés entre le thread de contrôle et les clients.
#[derive(Clone)]
pub struct Channels {
    pub commands: Sender<Command>,
    pub live: broadcast::Sender<String>,
    pub state: watch::Receiver<String>,
    /// Dernière liste d'appareils RGB (vide tant que la détection n'est pas finie).
    pub rgb: watch::Receiver<String>,
}

/// Un client : reçoit l'état courant, les appareils RGB, puis le flux live ; ses lignes sont des `ClientMsg`.
async fn handle_client<S: AsyncRead + AsyncWrite>(stream: S, channels: Channels) {
    let live_rx = channels.live.subscribe();
    let (reader, writer) = tokio::io::split(stream);
    tokio::select! {
        () = read_commands(reader, &channels) => {}
        () = write_stream(writer, channels.state.clone(), channels.rgb.clone(), live_rx) => {}
    }
    tracing::debug!("client déconnecté");
}

async fn write_stream<W: AsyncWrite + Unpin>(
    mut writer: W,
    state: watch::Receiver<String>,
    rgb: watch::Receiver<String>,
    mut live: broadcast::Receiver<String>,
) {
    let first = state.borrow().clone() + rgb.borrow().as_str();
    if writer.write_all(first.as_bytes()).await.is_err() {
        return;
    }
    loop {
        let line = match live.recv().await {
            Ok(line) => line,
            Err(broadcast::error::RecvError::Lagged(_)) => continue, // client lent : on saute
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if writer.write_all(line.as_bytes()).await.is_err() {
            return;
        }
    }
}

async fn read_commands<R: AsyncRead + Unpin>(reader: R, channels: &Channels) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let limit = (MAX_LINE + 1) as u64;
        match (&mut reader).take(limit).read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if buf.len() > MAX_LINE && buf.last() != Some(&b'\n') {
            tracing::warn!("commande client > 1 Mio, client déconnecté");
            return;
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            reply_error(channels, "commande non UTF-8".into());
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        match ipc::decode::<ClientMsg>(line) {
            Ok(msg) => {
                if channels.commands.send(Command::Client(msg)).is_err() {
                    return; // thread de contrôle arrêté
                }
            }
            Err(err) => reply_error(channels, format!("commande illisible : {err}")),
        }
    }
}

fn reply_error(channels: &Channels, message: String) {
    tracing::warn!("{message}");
    if let Ok(line) = ipc::encode(&DaemonMsg::Error { message }) {
        let _ = channels.live.send(line);
    }
}

#[cfg(windows)]
pub async fn serve(channels: Channels) -> anyhow::Result<()> {
    let security = windows_pipe::PipeSecurity::new()?;
    let mut server = security.create(true)?;
    tracing::info!("écoute sur {}", ipc::PIPE_NAME);
    loop {
        server.connect().await?;
        // Créer l'instance suivante avant de céder la connectée : aucun client ne trouve le pipe absent.
        let connected = std::mem::replace(&mut server, security.create(false)?);
        tokio::spawn(handle_client(connected, channels.clone()));
    }
}

#[cfg(windows)]
mod windows_pipe {
    use hub_core::ipc::PIPE_NAME;
    use std::io;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::core::w;

    /// DACL : SYSTEM et Administrateurs en total, utilisateurs interactifs en lecture/écriture
    /// (l'UI non élevée envoie des commandes ; le daemon les re-valide toutes).
    pub struct PipeSecurity(PSECURITY_DESCRIPTOR);

    impl PipeSecurity {
        pub fn new() -> io::Result<Self> {
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            // SAFETY: chaîne SDDL statique valide, pointeur de sortie local ; libéré dans Drop.
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    w!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)"),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    None,
                )?;
            }
            Ok(Self(descriptor))
        }

        pub fn create(&self, first: bool) -> io::Result<NamedPipeServer> {
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.0.0,
                bInheritHandle: false.into(),
            };
            // SAFETY: `attributes` et le descripteur vivent pendant l'appel (CreateNamedPipeW les copie).
            unsafe {
                ServerOptions::new()
                    .first_pipe_instance(first)
                    .create_with_security_attributes_raw(PIPE_NAME, (&raw mut attributes).cast())
            }
        }
    }

    impl Drop for PipeSecurity {
        fn drop(&mut self) {
            // SAFETY: alloué par ConvertStringSecurityDescriptorToSecurityDescriptorW (LocalAlloc).
            unsafe {
                LocalFree(Some(HLOCAL(self.0.0)));
            }
        }
    }
}

#[cfg(unix)]
pub async fn serve(channels: Channels) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use tokio::net::UnixListener;

    let path = Path::new(ipc::SOCKET_PATH);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err.into()),
        _ => {}
    }
    let listener = UnixListener::bind(path)?;
    // Toute commande est re-validée par le daemon ; groupe dédié au packaging.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    tracing::info!("écoute sur {}", ipc::SOCKET_PATH);
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(handle_client(stream, channels.clone()));
    }
}
