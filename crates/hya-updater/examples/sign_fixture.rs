//! Demo-only metadata signer for docs/examples/self-update.
//! Never use the fixed demo key for production releases.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ed25519_dalek::{Signer, SigningKey};
use hya_updater::{
    ArtifactDigest, ReleaseMetadata, SUPPORTED_PROTOCOL_VERSION, TrustRoot, write_trust_roots,
};
use sha2::{Digest, Sha256};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    sequence: u64,
    #[arg(long)]
    platform: String,
    #[arg(long = "artifact", required = true)]
    artifacts: Vec<String>,
    #[arg(long)]
    package: PathBuf,
    #[arg(long)]
    write_roots: PathBuf,
    #[arg(long, default_value = "demo-ci")]
    key_id: String,
}

fn main() -> ExitCode {
    let args = Args::parse();
    // Fixed demo seed — not a production secret.
    let signing = SigningKey::from_bytes(&[42u8; 32]);
    let mut digests = Vec::new();
    for name in &args.artifacts {
        let bytes = fs::read(args.package.join(name)).expect("read artifact");
        let digest = Sha256::digest(&bytes);
        digests.push(ArtifactDigest {
            name: name.clone(),
            size: bytes.len() as u64,
            sha256_hex: digest.iter().map(|b| format!("{b:02x}")).collect(),
        });
    }
    let mut metadata = ReleaseMetadata {
        sequence: args.sequence,
        platform: args.platform,
        artifacts: digests,
        not_before: 0,
        not_after: i64::MAX,
        recovery: false,
        protocol_version: SUPPORTED_PROTOCOL_VERSION,
        min_updater_version: "0.34.0".to_string(),
        key_id: args.key_id.clone(),
        signature: Vec::new(),
    };
    let payload = hya_updater::canonical_metadata_payload(&metadata).expect("payload");
    metadata.signature = signing.sign(&payload).to_bytes().to_vec();
    let json = serde_json::to_string_pretty(&metadata).expect("json");
    fs::write(&args.out, format!("{json}\n")).expect("write metadata");
    write_trust_roots(
        &args.write_roots,
        &[TrustRoot {
            key_id: args.key_id,
            verifying_key: signing.verifying_key().to_bytes(),
        }],
    )
    .expect("write roots");
    println!("wrote {}", args.out.display());
    ExitCode::SUCCESS
}
