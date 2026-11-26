//! Run editor-provided commands in the adapter, with the editor session cwd.
//! Only connected MCP byte streams cross into the shared backend.
use std::{os::unix::fs::PermissionsExt, path::Path, process::Stdio};

use agent_client_protocol::v1::schema as acp;
use anyhow::{Context, ensure};
use hya_api::v1 as pb;
use tokio::{net::UnixListener, process::Command, task::JoinSet};

pub(super) struct Peers {
    _tasks: JoinSet<anyhow::Result<()>>,
    pub(super) disconnected: tokio_util::sync::CancellationToken,
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
        let mut peers = Vec::new();
        let mut tasks = JoinSet::new();
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
            tasks.spawn(async move {
                let name = server.name.clone();
                // The backend cannot see spawn errors; stderr is the editor's agent log.
                bridge(listener, server, cwd)
                    .await
                    .inspect_err(|error| eprintln!("hya acp: MCP server `{name}`: {error:#}"))
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
        let disconnected = tokio_util::sync::CancellationToken::new();
        let closed = disconnected.clone();
        tasks.spawn(async move {
            let result = lease.message().await;
            closed.cancel();
            result?;
            Ok(())
        });
        Ok((
            id,
            Self {
                _tasks: tasks,
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
) -> anyhow::Result<()> {
    let (stream, _) = listener.accept().await?;
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
    }
    child.kill().await?;
    Ok(())
}
