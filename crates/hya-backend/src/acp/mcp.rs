//! Run editor-provided commands in the adapter, with the editor session cwd.
//! Only connected MCP byte streams cross into the shared backend.
use std::{os::unix::fs::PermissionsExt, path::Path, process::Stdio};

use agent_client_protocol::v1::schema as acp;
use anyhow::{Context, ensure};
use hya_api::v1 as pb;
use tokio::{net::UnixListener, process::Command};
use tokio_util::sync::{CancellationToken, DropGuard};

pub(super) struct Peers {
    /// Dropping it stops every bridge, which then kills and reaps its server.
    /// Aborting the bridges instead would leave each killed server a zombie
    /// until tokio's best-effort background reaping gets to it.
    _stop: DropGuard,
    pub(super) disconnected: CancellationToken,
}

impl Peers {
    pub(super) async fn open(
        url: &str,
        cwd: &Path,
        model: String,
        servers: Vec<acp::McpServer>,
    ) -> anyhow::Result<(String, Self)> {
        let dir = tempfile::Builder::new()
            .prefix("hya-acp-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")?;
        let stop = CancellationToken::new();
        let guard = stop.clone().drop_guard();
        let mut peers = Vec::new();
        for server in servers {
            let acp::McpServer::Stdio(server) = server else {
                anyhow::bail!("only stdio MCP peers are supported")
            };
            ensure!(
                server.command.is_absolute(),
                "MCP command must be an absolute path"
            );
            let socket = dir
                .path()
                .join(format!("peer-{}", hya_proto::SessionId::new()));
            let listener = UnixListener::bind(&socket)?;
            peers.push(pb::SessionMcpPeer {
                name: server.name.clone(),
                socket: socket.to_string_lossy().into_owned(),
            });
            let cwd = cwd.to_path_buf();
            let stop = stop.clone();
            tokio::spawn(async move {
                let name = server.name.clone();
                // The backend cannot see spawn errors; stderr is the editor's agent log.
                if let Err(error) = bridge(listener, server, cwd, stop).await {
                    eprintln!("hya acp: MCP server `{name}`: {error:#}");
                }
            });
        }
        let channel = tonic::transport::Endpoint::from_shared(url.to_owned())?
            .connect()
            .await?;
        let mut lease = pb::mcp_client::McpClient::new(channel)
            .open_mcp_session(pb::OpenMcpSessionRequest {
                workdir: cwd.to_string_lossy().into_owned(),
                model,
                peers,
            })
            .await?
            .into_inner();
        let id = lease
            .message()
            .await?
            .context("MCP lease ended before session creation")?
            .session;
        let disconnected = CancellationToken::new();
        let closed = disconnected.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = lease.message() => closed.cancel(),
                () = stop.cancelled() => {}
            }
        });
        Ok((
            id,
            Self {
                _stop: guard,
                disconnected,
            },
        ))
    }
}

/// Accept the backend's connection, then run the server and pipe its stdio.
async fn bridge(
    listener: UnixListener,
    server: acp::McpServerStdio,
    cwd: std::path::PathBuf,
    stop: CancellationToken,
) -> anyhow::Result<()> {
    let (stream, _) = tokio::select! {
        accepted = listener.accept() => accepted?,
        () = stop.cancelled() => return Ok(()),
    };
    // SAFETY: geteuid takes no arguments and cannot fail.
    ensure!(
        stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
        "unexpected MCP peer"
    );
    let command = server.command;
    let mut child = Command::new(&command)
        .args(server.args)
        .envs(server.env.into_iter().map(|v| (v.name, v.value)))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("start {}", command.display()))?;
    let (mut read, mut write) = stream.into_split();
    let mut input = child.stdin.take().context("missing MCP input")?;
    let mut output = child.stdout.take().context("missing MCP output")?;
    tokio::select! {
        r = tokio::io::copy(&mut read, &mut input) => { r?; },
        r = tokio::io::copy(&mut output, &mut write) => { r?; },
        r = child.wait() => {
            let status = r?;
            ensure!(status.success(), "exited with {status}");
            return Ok(());
        }
        () = stop.cancelled() => {}
    }
    child.kill().await?;
    Ok(())
}
