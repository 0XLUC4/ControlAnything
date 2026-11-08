//! Local IPC server: broadcasts the state and live stream, forwards commands to the control thread.

use crate::control::Command;
use hub_core::ipc::{self, ClientMsg, DaemonMsg};
use std::sync::mpsc::Sender;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, watch};

/// Maximum size of a client command; beyond it, the client is disconnected.
const MAX_LINE: usize = 1024 * 1024;

/// Channels shared between the control thread and the clients.
#[derive(Clone)]
pub struct Channels {
    pub commands: Sender<Command>,
    pub live: broadcast::Sender<String>,
    pub state: watch::Receiver<String>,
    /// Latest RGB device list (empty until detection is done).
    pub rgb: watch::Receiver<String>,
}

/// One client: receives the current state, the RGB devices, then the live stream; its lines are `ClientMsg`.
async fn handle_client<S: AsyncRead + AsyncWrite>(stream: S, channels: Channels) {
    let live_rx = channels.live.subscribe();
    let (reader, writer) = tokio::io::split(stream);
    tokio::select! {
        () = read_commands(reader, &channels) => {}
        () = write_stream(writer, channels.state.clone(), channels.rgb.clone(), live_rx) => {}
    }
    tracing::debug!("client disconnected");
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
            Err(broadcast::error::RecvError::Lagged(_)) => continue, // slow client: skip
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
            tracing::warn!("client command > 1 MiB, client disconnected");
            return;
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            reply_error(channels, "non UTF-8 command".into());
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        match ipc::decode::<ClientMsg>(line) {
            Ok(msg) => {
                if channels.commands.send(Command::Client(msg)).is_err() {
                    return; // control thread stopped
                }
            }
            Err(err) => reply_error(channels, format!("unreadable command: {err}")),
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
    tracing::info!("listening on {}", ipc::PIPE_NAME);
    loop {
        server.connect().await?;
        // Create the next instance before handing off the connected one: no client finds the pipe missing.
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

    /// DACL: full access for SYSTEM and Administrators, read/write for interactive users
    /// (the non-elevated UI sends commands; the daemon re-validates all of them).
    pub struct PipeSecurity(PSECURITY_DESCRIPTOR);

    impl PipeSecurity {
        pub fn new() -> io::Result<Self> {
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            // SAFETY: valid static SDDL string, local output pointer; freed in Drop.
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
            // SAFETY: `attributes` and the descriptor live during the call (CreateNamedPipeW copies them).
            unsafe {
                ServerOptions::new()
                    .first_pipe_instance(first)
                    .create_with_security_attributes_raw(PIPE_NAME, (&raw mut attributes).cast())
            }
        }
    }

    impl Drop for PipeSecurity {
        fn drop(&mut self) {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW (LocalAlloc).
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
    // Every command is re-validated by the daemon; a dedicated group is left to packaging.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    tracing::info!("listening on {}", ipc::SOCKET_PATH);
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(handle_client(stream, channels.clone()));
    }
}
