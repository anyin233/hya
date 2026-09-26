//! `gen-relay` — regenerate the `hya.relay.v1` rendezvous protocol types.
//!
//! Runs the vendored `protoc` over `proto/hya/relay/v1/*.proto` and emits
//! prost types plus tonic clients/servers into `crates/hya-relay/src/gen/`
//! (committed; normal builds need no protoc). The relay protocol is binary
//! only (gRPC and WebSocket binary frames), so no protojson serde, API
//! reference, or OpenAPI output is produced.

use std::fs;

use anyhow::{Context, Result, bail};
use prost::Message;

use crate::gen_api::{run_protoc_descriptor, workspace_root};

/// Entry point for `cargo xtask gen-relay`.
pub fn run(_args: Vec<String>) -> Result<()> {
    let root = workspace_root()?;
    let proto_dir = root.join("proto");
    let relay_dir = proto_dir.join("hya").join("relay").join("v1");
    let gen_dir = root
        .join("crates")
        .join("hya-relay")
        .join("src")
        .join("gen");
    fs::create_dir_all(&gen_dir).context("create gen dir")?;
    for entry in fs::read_dir(&gen_dir).context("list gen dir")? {
        let path = entry.context("read gen dir entry")?.path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
    }

    let mut protos = Vec::new();
    for entry in fs::read_dir(&relay_dir).context("list relay proto dir")? {
        let path = entry.context("read relay proto dir entry")?.path();
        if path.extension().is_some_and(|ext| ext == "proto") {
            protos.push(path);
        }
    }
    protos.sort();
    if protos.is_empty() {
        bail!("no .proto files under {}", relay_dir.display());
    }

    let protoc = protoc_bin_vendored::protoc_bin_path()
        .map_err(|error| anyhow::anyhow!("resolve vendored protoc: {error}"))?;
    // The descriptor set is a throwaway intermediate; keep it out of the
    // repository and out of any Cargo target directory.
    let scratch = std::env::temp_dir().join(format!("hya-gen-relay-{}", std::process::id()));
    fs::create_dir_all(&scratch).context("create scratch dir")?;
    let descriptor_path = scratch.join("hya-relay-v1-descriptor.bin");
    let result = (|| -> Result<()> {
        let descriptor = run_protoc_descriptor(&protoc, &proto_dir, &protos, &descriptor_path)?;
        let fds = prost_types::FileDescriptorSet::decode(descriptor.as_slice())
            .context("decode descriptor set")?;
        tonic_build::configure()
            .build_client(true)
            .build_server(true)
            .out_dir(&gen_dir)
            .compile_fds(fds)
            .context("tonic/prost codegen")
    })();
    let _ = fs::remove_dir_all(&scratch);
    result?;

    println!("gen-relay: regenerated {}", gen_dir.display());
    Ok(())
}
