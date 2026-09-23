//! `gen-api` — regenerate the hya v1 dual-protocol contract.
//!
//! Runs the vendored `protoc` over `proto/hya/v1/*.proto` and emits:
//!
//! - Rust types + tonic clients/servers + pbjson protojson serde into
//!   `crates/hya-api/src/gen/` (committed; normal builds need no protoc),
//! - `docs/protocol/api-reference.md` and `docs/protocol/openapi.json`
//!   derived from the same sources,
//! - a coverage check that every rpc declares exactly one
//!   `// hya.http: METHOD /path` mapping and that no mapping collides.
//!
//! `METHOD` is `GET`, `POST`, `PUT`, `PATCH`, or `DELETE`, or the catch-all
//! `ANY`, which binds all five on one path (used by the bundle API
//! passthrough rpcs, whose request message carries the method explicitly).
//! An `ANY` mapping collides with every explicit method on the same path, and
//! OpenAPI output expands it into five operations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use prost::Message;

/// Entry point for `cargo xtask gen-api`.
pub fn run(_args: Vec<String>) -> Result<()> {
    let root = workspace_root()?;
    let proto_dir = root.join("proto");
    let v1_dir = proto_dir.join("hya").join("v1");
    let gen_dir = root.join("crates").join("hya-api").join("src").join("gen");
    fs::create_dir_all(&gen_dir).context("create gen dir")?;
    for entry in fs::read_dir(&gen_dir).context("list gen dir")? {
        let path = entry.context("read gen dir entry")?.path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
    }

    let protos = proto_files(&v1_dir)?;
    if protos.is_empty() {
        bail!("no .proto files under {}", v1_dir.display());
    }

    let protoc = protoc_bin_vendored::protoc_bin_path()
        .map_err(|error| anyhow::anyhow!("resolve vendored protoc: {error}"))?;

    let target = root.join("target").join("xtask");
    fs::create_dir_all(&target).context("create target/xtask")?;
    let descriptor_path = target.join("hya-v1-descriptor.bin");
    let descriptor = run_protoc_descriptor(&protoc, &proto_dir, &protos, &descriptor_path)?;
    fs::write(&descriptor_path, &descriptor).context("write descriptor set")?;

    let fds = prost_types::FileDescriptorSet::decode(descriptor.as_slice())
        .context("decode descriptor set")?;
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_well_known_types(true)
        .extern_path(".google.protobuf", "::pbjson_types")
        .out_dir(&gen_dir)
        .compile_fds(fds)
        .context("tonic/prost codegen")?;

    pbjson_build::Builder::new()
        .register_descriptors(descriptor.as_slice())
        .context("register descriptors")?
        .out_dir(&gen_dir)
        .build(&[".hya.v1"])
        .context("pbjson serde codegen")?;

    let contract = parse_contract(&protos)?;
    let docs_dir = root.join("docs").join("protocol");
    fs::create_dir_all(&docs_dir).context("create docs/protocol")?;
    write_api_reference(&docs_dir.join("api-reference.md"), &contract)?;
    write_openapi(&docs_dir.join("openapi.json"), &contract)?;

    let mut emitted: Vec<String> = Vec::new();
    for entry in fs::read_dir(&gen_dir).context("list gen dir")? {
        let path = entry.context("read gen dir entry")?.path();
        if path.extension().is_some_and(|ext| ext == "rs")
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            emitted.push(name.to_owned());
        }
    }
    emitted.sort();
    println!("gen-api: emitted {} gen files", emitted.len());
    println!(
        "gen-api: {} services, {} rpcs, {} messages",
        contract.services.len(),
        contract
            .services
            .iter()
            .map(|s| s.rpcs.len())
            .sum::<usize>(),
        contract.messages.len(),
    );
    Ok(())
}

/// Run the vendored protoc to produce a self-contained descriptor set.
///
/// `--include_imports` pulls in the well-known types so pbjson can resolve
/// them; `--include_source_info` preserves the `//` comments that prost and
/// tonic copy into the generated Rust docs.
fn run_protoc_descriptor(
    protoc: &Path,
    proto_dir: &Path,
    protos: &[PathBuf],
    descriptor_out: &Path,
) -> Result<Vec<u8>> {
    let include = protoc_bin_vendored::include_path()
        .map_err(|error| anyhow::anyhow!("resolve vendored protoc include: {error}"))?;
    let mut command = std::process::Command::new(protoc);
    command
        .arg("--include_imports")
        .arg("--include_source_info")
        .arg("--proto_path")
        .arg(proto_dir)
        .arg("--proto_path")
        .arg(&include)
        .arg("--descriptor_set_out")
        .arg(descriptor_out);
    for proto in protos {
        command.arg(proto);
    }
    let output = command.output().context("spawn vendored protoc")?;
    if !output.status.success() {
        bail!(
            "protoc failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::read(descriptor_out).context("read generated descriptor set")
}

fn workspace_root() -> Result<PathBuf> {
    let dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let mut current = dir.as_path();
    loop {
        if current.join("Cargo.toml").is_file() && current.join("crates").is_dir() {
            return Ok(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => bail!("workspace root not found"),
        }
    }
}

fn proto_files(v1_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut protos = Vec::new();
    for entry in fs::read_dir(v1_dir).context("list proto dir")? {
        let path = entry.context("read proto dir entry")?.path();
        if path.extension().is_some_and(|ext| ext == "proto") {
            protos.push(path);
        }
    }
    protos.sort();
    Ok(protos)
}

/// Parsed contract model used for documentation generation.
struct Contract {
    services: Vec<ServiceDoc>,
    messages: Vec<MessageDoc>,
    enums: Vec<EnumDoc>,
}

struct ServiceDoc {
    name: String,
    comment: Vec<String>,
    rpcs: Vec<RpcDoc>,
}

struct RpcDoc {
    name: String,
    comment: Vec<String>,
    method: String,
    path: String,
    input: String,
    output: String,
    server_streaming: bool,
}

struct MessageDoc {
    name: String,
    comment: Vec<String>,
    fields: Vec<FieldDoc>,
}

struct FieldDoc {
    name: String,
    number: String,
    kind: String,
    label: String,
    oneof: Option<String>,
    comment: Vec<String>,
}

struct EnumDoc {
    name: String,
    comment: Vec<String>,
    values: Vec<EnumValueDoc>,
}

struct EnumValueDoc {
    name: String,
    number: String,
    comment: Vec<String>,
}

fn parse_contract(protos: &[PathBuf]) -> Result<Contract> {
    let mut contract = Contract {
        services: Vec::new(),
        messages: Vec::new(),
        enums: Vec::new(),
    };
    let mut seen_paths: BTreeMap<(String, String), String> = BTreeMap::new();

    for proto in protos {
        let text =
            fs::read_to_string(proto).with_context(|| format!("read {}", proto.display()))?;

        let mut comment: Vec<String> = Vec::new();
        let mut http: Option<(String, String)> = None;
        // (depth at declaration, index into contract vectors, oneof stack)
        let mut message_stack: Vec<(usize, usize)> = Vec::new();
        let mut enum_stack: Vec<(usize, usize)> = Vec::new();
        let mut service_depth: Option<usize> = None;
        let mut depth = 0usize;
        let mut oneof_stack: Vec<(usize, String)> = Vec::new();

        for raw in text.lines() {
            let line = raw.trim();
            if line.starts_with("//") {
                let body = line.trim_start_matches('/').trim();
                if let Some(mapping) = body.strip_prefix("hya.http:") {
                    let mapping = mapping.trim();
                    let Some((method, path)) = mapping.split_once(' ') else {
                        bail!(
                            "malformed hya.http mapping in {}: {mapping}",
                            proto.display()
                        );
                    };
                    if method != ANY_METHOD && !HTTP_METHODS.contains(&method) {
                        bail!(
                            "unsupported hya.http method `{method}` in {} (expected one of {} or {ANY_METHOD})",
                            proto.display(),
                            HTTP_METHODS.join(", ")
                        );
                    }
                    http = Some((method.to_owned(), path.to_owned()));
                } else {
                    comment.push(body.to_owned());
                }
                continue;
            }
            if line.starts_with("syntax")
                || line.starts_with("package")
                || line.starts_with("import")
            {
                comment.clear();
                http = None;
                continue;
            }

            if line.starts_with("service ") && line.ends_with('{') {
                let name = declaration_name(line, "service ");
                contract.services.push(ServiceDoc {
                    name: name.clone(),
                    comment: std::mem::take(&mut comment),
                    rpcs: Vec::new(),
                });
                service_depth = Some(depth);
                depth += 1;
                continue;
            }
            if line.starts_with("rpc ") {
                let (name, input, output, server_streaming) = parse_rpc_line(line)?;
                let index = contract
                    .services
                    .len()
                    .checked_sub(1)
                    .context("rpc outside service")?;
                let Some((method, path)) = http.take() else {
                    bail!(
                        "rpc {}.{} has no `// hya.http:` mapping ({})",
                        contract.services[index].name,
                        name,
                        proto.display()
                    );
                };
                for bound in expand_method(&method) {
                    if let Some(previous) = seen_paths.insert(
                        (bound.to_owned(), path.clone()),
                        format!("{}.{}", contract.services[index].name, name),
                    ) {
                        bail!(
                            "duplicate HTTP mapping {bound} {path}: {previous} and {}.{}",
                            contract.services[index].name,
                            name
                        );
                    }
                }
                contract.services[index].rpcs.push(RpcDoc {
                    name,
                    comment: std::mem::take(&mut comment),
                    method,
                    path,
                    input,
                    output,
                    server_streaming,
                });
                comment.clear();
                continue;
            }
            if line.starts_with("message ") && line.ends_with('{') {
                let name = declaration_name(line, "message ");
                contract.messages.push(MessageDoc {
                    name: name.clone(),
                    comment: std::mem::take(&mut comment),
                    fields: Vec::new(),
                });
                message_stack.push((depth, contract.messages.len() - 1));
                depth += 1;
                continue;
            }
            if line.starts_with("enum ") && line.ends_with('{') {
                let name = declaration_name(line, "enum ");
                contract.enums.push(EnumDoc {
                    name: name.clone(),
                    comment: std::mem::take(&mut comment),
                    values: Vec::new(),
                });
                enum_stack.push((depth, contract.enums.len() - 1));
                depth += 1;
                continue;
            }
            if line.starts_with("oneof ") && line.ends_with('{') {
                let name = declaration_name(line, "oneof ");
                oneof_stack.push((depth, name));
                depth += 1;
                continue;
            }
            if line == "}" || line == "};" {
                if let Some(&(d, _)) = message_stack.last()
                    && d == depth.saturating_sub(1)
                {
                    message_stack.pop();
                }
                if let Some(&(d, _)) = enum_stack.last()
                    && d == depth.saturating_sub(1)
                {
                    enum_stack.pop();
                }
                if let Some(&(d, _)) = oneof_stack.last()
                    && d == depth.saturating_sub(1)
                {
                    oneof_stack.pop();
                }
                if service_depth == Some(depth.saturating_sub(1)) {
                    service_depth = None;
                }
                depth = depth.saturating_sub(1);
                comment.clear();
                continue;
            }

            // Field or enum-value line inside an open declaration.
            if let Some(&(_, index)) = message_stack.last() {
                if let Some(mut field) = parse_field_line(line, oneof_stack.last().map(|(_, n)| n))
                {
                    field.comment = std::mem::take(&mut comment);
                    contract.messages[index].fields.push(field);
                }
            } else if let Some(&(_, index)) = enum_stack.last()
                && let Some((name, number)) = parse_enum_value_line(line)
            {
                contract.enums[index].values.push(EnumValueDoc {
                    name,
                    number,
                    comment: std::mem::take(&mut comment),
                });
            }
        }
    }
    Ok(contract)
}

/// Explicit HTTP methods an rpc may bind.
const HTTP_METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

/// Catch-all binding: every method in [`HTTP_METHODS`] on one path.
const ANY_METHOD: &str = "ANY";

/// The concrete methods one `hya.http` mapping binds.
fn expand_method(method: &str) -> Vec<&str> {
    if method == ANY_METHOD {
        HTTP_METHODS.to_vec()
    } else {
        vec![method]
    }
}

fn declaration_name(line: &str, keyword: &str) -> String {
    line.trim_start_matches(keyword)
        .trim_end_matches('{')
        .trim()
        .to_owned()
}

fn parse_rpc_line(line: &str) -> Result<(String, String, String, bool)> {
    // Shape: rpc Name(Req) returns (stream Resp);
    let line = line.trim_end_matches(';').trim();
    let Some(open) = line.find('(') else {
        bail!("malformed rpc line: {line}");
    };
    let name = line["rpc ".len()..open].trim().to_owned();
    let rest = &line[open + 1..];
    let Some(close) = rest.find(')') else {
        bail!("malformed rpc line: {line}");
    };
    let input = rest[..close].trim().to_owned();
    let tail = rest[close + 1..].trim();
    let Some(returns_at) = tail.find("returns") else {
        bail!("malformed rpc line: {line}");
    };
    let mut output = tail[returns_at + "returns".len()..]
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim()
        .to_owned();
    let server_streaming = output.strip_prefix("stream ").is_some();
    if server_streaming {
        output = output
            .strip_prefix("stream ")
            .unwrap_or_default()
            .trim()
            .to_owned();
    }
    Ok((name, input, output, server_streaming))
}

fn parse_field_line(line: &str, oneof: Option<&String>) -> Option<FieldDoc> {
    let line = line.trim_end_matches(';').trim();
    let eq = line.rfind('=')?;
    let number = line[eq + 1..].trim().to_owned();
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut left = line[..eq].trim();
    let mut label = String::new();
    for prefix in ["optional ", "repeated "] {
        if let Some(rest) = left.strip_prefix(prefix) {
            label = prefix.trim().to_owned();
            left = rest.trim();
            break;
        }
    }
    let kind_start = left.find(' ')?;
    let (kind, name) = left.split_at(kind_start);
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return None;
    }
    Some(FieldDoc {
        name: name.to_owned(),
        number,
        kind: kind.trim().to_owned(),
        label,
        oneof: oneof.cloned(),
        comment: Vec::new(),
    })
}

fn parse_enum_value_line(line: &str) -> Option<(String, String)> {
    let line = line.trim_end_matches(';').trim();
    let eq = line.find('=')?;
    let name = line[..eq].trim();
    let number = line[eq + 1..].trim();
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some((name.to_owned(), number.to_owned()))
}

fn write_api_reference(path: &Path, contract: &Contract) -> Result<()> {
    let mut out = String::new();
    out.push_str("# hya API v1 Reference\n\n");
    out.push_str(
        "Generated from `proto/hya/v1` by `cargo xtask gen-api`; do not edit by hand.\n\
         The same contract is served over HTTP/JSON+SSE and gRPC. Every rpc lists its\n\
         HTTP binding (`method path`) and its fully-qualified gRPC method\n\
         (`hya.v1.<Service>.<Rpc>`).\n\n\
         Conventions: pagination uses opaque cursors; errors use the stable code\n\
         table (`hya_api::error`); timestamps are RFC 3339 strings in JSON. An `ANY`\n\
         binding accepts GET, POST, PUT, PATCH, and DELETE on one path (the request\n\
         message names the method), and its trailing `{path}` spans every remaining\n\
         path segment.\n\n",
    );
    out.push_str("## Contents\n\n");
    for service in &contract.services {
        out.push_str(&format!(
            "- [{} service](#service-{})\n",
            service.name,
            slug(&service.name)
        ));
    }
    out.push_str("- [Messages](#messages)\n- [Enums](#enums)\n\n---\n\n");

    for service in &contract.services {
        out.push_str(&format!("## Service `{}`\n\n", service.name));
        for line in &service.comment {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("\n| RPC | HTTP | gRPC | Request | Response |\n|---|---|---|---|---|\n");
        for rpc in &service.rpcs {
            let streaming = if rpc.server_streaming {
                " (stream)"
            } else {
                ""
            };
            out.push_str(&format!(
                "| `{}` | `{} {}{}` | `hya.v1.{}.{}` | `{}` | `{}` |\n",
                rpc.name,
                rpc.method,
                rpc.path,
                streaming,
                service.name,
                rpc.name,
                trim_type(&rpc.input),
                trim_type(&rpc.output),
            ));
        }
        out.push('\n');
        for rpc in &service.rpcs {
            if rpc.comment.is_empty() {
                continue;
            }
            out.push_str(&format!("### `{}.{}`\n\n", service.name, rpc.name));
            for line in &rpc.comment {
                out.push_str(line);
                out.push('\n');
            }
            out.push('\n');
        }
    }

    out.push_str("## Messages\n\n");
    for message in &contract.messages {
        out.push_str(&format!("### `{}`\n\n", message.name));
        for line in &message.comment {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("\n| Field | Type | Description |\n|---|---|---|\n");
        for field in &message.fields {
            let mut kind = field.kind.clone();
            if !field.label.is_empty() {
                kind = format!("{} {}", field.label, kind);
            }
            if let Some(oneof) = &field.oneof {
                kind = format!("oneof `{oneof}`: {kind}");
            }
            let description = field.comment.join(" ");
            out.push_str(&format!(
                "| `{}` ({}) | `{}` | {} |\n",
                field.name, field.number, kind, description
            ));
        }
        out.push('\n');
    }

    out.push_str("## Enums\n\n");
    for en in &contract.enums {
        out.push_str(&format!("### `{}`\n\n", en.name));
        for line in &en.comment {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("\n| Value | Number | Description |\n|---|---|---|\n");
        for value in &en.values {
            let description = value.comment.join(" ");
            out.push_str(&format!(
                "| `{}` | {} | {} |\n",
                value.name, value.number, description
            ));
        }
        out.push('\n');
    }

    fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn trim_type(name: &str) -> String {
    name.trim_start_matches("hya.v1.").to_owned()
}

fn slug(name: &str) -> String {
    name.to_lowercase()
}

fn write_openapi(path: &Path, contract: &Contract) -> Result<()> {
    let mut paths: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for service in &contract.services {
        for rpc in &service.rpcs {
            let any = rpc.method == ANY_METHOD;
            for method in expand_method(&rpc.method) {
                let method = method.to_lowercase();
                let operation_id = if any {
                    format!("{}.{}.{method}", service.name, rpc.name)
                } else {
                    format!("{}.{}", service.name, rpc.name)
                };
                let mut operation = serde_json::json!({
                    "operationId": operation_id,
                    "summary": rpc.comment.first().cloned().unwrap_or_default(),
                    "tags": [service.name],
                    "x-grpc-method": format!("hya.v1.{}.{}", service.name, rpc.name),
                    "x-server-streaming": rpc.server_streaming,
                });
                if any && let Some(object) = operation.as_object_mut() {
                    object.insert("x-hya-any-method".to_owned(), serde_json::Value::Bool(true));
                }
                paths
                    .entry(rpc.path.clone())
                    .or_default()
                    .insert(method, operation);
            }
        }
    }
    let paths: serde_json::Map<String, serde_json::Value> = paths
        .into_iter()
        .map(|(path, ops)| (path, serde_json::Value::Object(ops)))
        .collect();

    let mut schemas = serde_json::Map::new();
    for message in &contract.messages {
        let mut properties = serde_json::Map::new();
        for field in &message.fields {
            properties.insert(field.name.clone(), json_schema_type(field));
        }
        schemas.insert(
            message.name.clone(),
            serde_json::json!({
                "type": "object",
                "description": message.comment.join(" "),
                "properties": properties,
            }),
        );
    }
    for en in &contract.enums {
        let values: Vec<String> = en.values.iter().map(|v| v.name.clone()).collect();
        schemas.insert(
            en.name.clone(),
            serde_json::json!({
                "type": "string",
                "description": en.comment.join(" "),
                "enum": values,
            }),
        );
    }

    let doc = serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "hya API v1",
            "summary": "Generated from proto/hya/v1 by cargo xtask gen-api; do not edit by hand.",
            "version": "1.0.0",
        },
        "paths": serde_json::Value::Object(paths),
        "components": { "schemas": serde_json::Value::Object(schemas) },
    });
    let rendered = serde_json::to_string_pretty(&doc).context("render openapi json")?;
    fs::write(path, rendered).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn json_schema_type(field: &FieldDoc) -> serde_json::Value {
    let repeated = field.label == "repeated";
    let base = match field.kind.as_str() {
        "string" => serde_json::json!({ "type": "string" }),
        "bool" => serde_json::json!({ "type": "boolean" }),
        "bytes" => serde_json::json!({ "type": "string", "contentEncoding": "base64" }),
        "uint32" | "uint64" | "int32" | "int64" => {
            serde_json::json!({ "type": "integer", "format": "int64" })
        }
        "float" | "double" => serde_json::json!({ "type": "number" }),
        "google.protobuf.Timestamp" => {
            serde_json::json!({ "type": "string", "format": "date-time" })
        }
        "google.protobuf.Struct" => serde_json::json!({ "type": "object" }),
        other => {
            if let Some((key, value)) = split_map_type(other) {
                serde_json::json!({
                    "type": "object",
                    "additionalProperties": primitive_schema(value.unwrap_or("string")),
                    "x-map-key": key.unwrap_or("string"),
                })
            } else {
                serde_json::json!({ "$ref": format!("#/components/schemas/{other}") })
            }
        }
    };
    if repeated {
        serde_json::json!({ "type": "array", "items": base })
    } else {
        base
    }
}

fn split_map_type(kind: &str) -> Option<(Option<&str>, Option<&str>)> {
    let inner = kind.strip_prefix("map<")?.strip_suffix('>')?;
    let (key, value) = inner.split_once(',')?;
    let key = key.trim();
    let value = value.trim();
    Some((
        if key.is_empty() { None } else { Some(key) },
        if value.is_empty() { None } else { Some(value) },
    ))
}

fn primitive_schema(kind: &str) -> serde_json::Value {
    match kind {
        "string" => serde_json::json!({ "type": "string" }),
        "bool" => serde_json::json!({ "type": "boolean" }),
        "uint32" | "uint64" | "int32" | "int64" => {
            serde_json::json!({ "type": "integer", "format": "int64" })
        }
        _ => serde_json::json!({ "type": "string" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract_of(name: &str, body: &str) -> Result<Contract> {
        let dir = std::env::temp_dir().join(format!("hya-gen-api-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        let proto = dir.join("demo.proto");
        fs::write(
            &proto,
            format!("syntax = \"proto3\";\npackage hya.v1;\n{body}"),
        )?;
        let contract = parse_contract(&[proto]);
        fs::remove_dir_all(&dir)?;
        contract
    }

    #[test]
    fn any_mapping_binds_every_method_and_collides_with_explicit_ones() -> Result<()> {
        let contract = contract_of(
            "any-ok",
            "service Demo {\n  // hya.http: ANY /v1/demo/{path}\n  rpc Invoke(A) returns (B);\n  // hya.http: GET /v1/demo\n  rpc List(A) returns (B);\n}\n",
        )?;
        assert_eq!(contract.services[0].rpcs[0].method, "ANY");
        let collision = contract_of(
            "any-collides",
            "service Demo {\n  // hya.http: ANY /v1/demo/{path}\n  rpc Invoke(A) returns (B);\n  // hya.http: PATCH /v1/demo/{path}\n  rpc Patch(A) returns (B);\n}\n",
        );
        let Err(error) = collision else {
            bail!("an ANY mapping must collide with PATCH on the same path");
        };
        assert!(
            error.to_string().contains("duplicate HTTP mapping PATCH"),
            "{error}"
        );
        let unknown = contract_of(
            "head",
            "service Demo {\n  // hya.http: HEAD /v1/demo\n  rpc Head(A) returns (B);\n}\n",
        );
        assert!(unknown.is_err(), "unsupported methods are rejected");
        Ok(())
    }
}
