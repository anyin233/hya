//! Startup waterfall bench for the 500ms full-sync / 100ms shell budgets.
//!
//! Modes:
//! - `backend` — spawn `hya-backend serve`, time until listen (and parse `HYA_STARTUP_TRACE`)
//! - `parse` — parse marks from stdin or a file (for tests / offline analysis)

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail};
use serde::Deserialize;
use serde_json::Value;

/// Run `cargo xtask startup-bench …`.
pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    let mut mode = "backend".to_string();
    let mut runs: usize = 5;
    let mut budget_backend_ms: Option<u64> = None;
    let mut backend_bin: Option<PathBuf> = None;
    let mut marks_file: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--mode" => {
                i += 1;
                mode = args
                    .get(i)
                    .context("--mode requires a value")?
                    .clone();
            }
            "--runs" => {
                i += 1;
                runs = args
                    .get(i)
                    .context("--runs requires a value")?
                    .parse()
                    .context("parse --runs")?;
            }
            "--budget-backend-ms" => {
                i += 1;
                budget_backend_ms = Some(
                    args.get(i)
                        .context("--budget-backend-ms requires a value")?
                        .parse()
                        .context("parse --budget-backend-ms")?,
                );
            }
            "--backend-bin" => {
                i += 1;
                backend_bin = Some(PathBuf::from(
                    args.get(i).context("--backend-bin requires a value")?,
                ));
            }
            "--marks-file" => {
                i += 1;
                marks_file = Some(PathBuf::from(
                    args.get(i).context("--marks-file requires a value")?,
                ));
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => bail!("unknown argument: {other}"),
        }
        i += 1;
    }

    match mode.as_str() {
        "backend" => run_backend_bench(runs, budget_backend_ms, backend_bin)?,
        "parse" => {
            let marks = match marks_file {
                Some(path) => parse_marks_file(&path)?,
                None => parse_marks_reader(std::io::stdin().lock())?,
            };
            println!("{}", serde_json::to_string_pretty(&summarize_marks(&marks))?);
        }
        other => bail!("unknown mode {other}; use backend|parse"),
    }
    Ok(())
}

fn print_help() {
    eprintln!(
        "usage: cargo xtask startup-bench [--mode backend|parse] [--runs N] \
         [--budget-backend-ms MS] [--backend-bin PATH] [--marks-file PATH]"
    );
}

#[derive(Debug, Clone, Deserialize)]
struct StartupMark {
    mark: String,
    wall_ms: u128,
    #[serde(default)]
    detail: Option<String>,
}

fn parse_mark_line(line: &str) -> Option<StartupMark> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("hya_startup") != Some(&Value::Bool(true)) {
        return None;
    }
    let mark = value.get("mark")?.as_str()?.to_string();
    let wall_ms = value.get("wall_ms")?.as_u64()? as u128;
    let detail = value
        .get("detail")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(StartupMark {
        mark,
        wall_ms,
        detail,
    })
}

/// Parse all startup marks from a reader.
fn parse_marks_reader(reader: impl BufRead) -> anyhow::Result<Vec<StartupMark>> {
    let mut marks = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if let Some(mark) = parse_mark_line(&line) {
            marks.push(mark);
        }
    }
    Ok(marks)
}

fn parse_marks_file(path: &Path) -> anyhow::Result<Vec<StartupMark>> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    parse_marks_reader(BufReader::new(file))
}

fn summarize_marks(marks: &[StartupMark]) -> Value {
    let t0 = marks.iter().map(|m| m.wall_ms).min().unwrap_or(0);
    let relative: Vec<Value> = marks
        .iter()
        .map(|m| {
            serde_json::json!({
                "mark": m.mark,
                "wall_ms": m.wall_ms,
                "delta_ms": m.wall_ms.saturating_sub(t0),
                "detail": m.detail,
            })
        })
        .collect();
    serde_json::json!({
        "t0_wall_ms": t0,
        "marks": relative,
    })
}

fn run_backend_bench(
    runs: usize,
    budget_ms: Option<u64>,
    backend_bin: Option<PathBuf>,
) -> anyhow::Result<()> {
    let bin = resolve_backend_bin(backend_bin)?;
    let mut samples = Vec::with_capacity(runs);
    for run in 0..runs {
        let sample = time_backend_ready(&bin)?;
        println!(
            "run {} backend_ready_ms={:.1} mark_delta_ms={}",
            run + 1,
            sample.ready_ms,
            sample
                .mark_delta_ms
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "n/a".into())
        );
        samples.push(sample.ready_ms);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p50 = percentile(&samples, 0.50);
    let p95 = percentile(&samples, 0.95);
    println!("backend_ready p50={p50:.1}ms p95={p95:.1}ms n={runs} bin={}", bin.display());
    if let Some(budget) = budget_ms {
        if p95 > budget as f64 {
            bail!("backend_ready p95 {p95:.1}ms exceeds budget {budget}ms");
        }
    }
    Ok(())
}

struct BackendSample {
    ready_ms: f64,
    mark_delta_ms: Option<f64>,
}

fn time_backend_ready(bin: &Path) -> anyhow::Result<BackendSample> {
    let tmp = tempfile_dir()?;
    let db = tmp.join("sessions.db");
    let t0 = Instant::now();
    let t0_wall = wall_ms();
    let mut child = Command::new(bin)
        .args([
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--db",
            &db.to_string_lossy(),
        ])
        .env("HYA_STARTUP_TRACE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;

    let stdout = child.stdout.take().context("stdout")?;
    let stderr = child.stderr.take().context("stderr")?;
    let (tx, rx) = std::sync::mpsc::channel::<LineMsg>();
    spawn_reader(stdout, LineSource::Stdout, tx.clone());
    spawn_reader(stderr, LineSource::Stderr, tx);

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready_ms = None;
    let mut mark_delta_ms = None;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining.min(Duration::from_millis(50))) {
            Ok((_source, line)) => {
                if ready_ms.is_none() && line.contains("listening on http://") {
                    ready_ms = Some(t0.elapsed().as_secs_f64() * 1000.0);
                }
                if let Some(mark) = parse_mark_line(&line) {
                    if mark.mark == "backend_listen" {
                        mark_delta_ms = Some(mark.wall_ms.saturating_sub(t0_wall) as f64);
                        if ready_ms.is_none() {
                            ready_ms = Some(t0.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                }
                // Prefer capturing the trace mark shortly after listen without padding every run.
                if ready_ms.is_some()
                    && (mark_delta_ms.is_some()
                        || t0.elapsed().as_secs_f64() * 1000.0
                            > ready_ms.unwrap_or(0.0) + 50.0)
                {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&tmp);

    let ready_ms = ready_ms.context("backend did not become ready within timeout")?;
    Ok(BackendSample {
        ready_ms,
        mark_delta_ms,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineSource {
    Stdout,
    Stderr,
}

type LineMsg = (LineSource, String);

fn spawn_reader(
    stream: impl std::io::Read + Send + 'static,
    source: LineSource,
    tx: std::sync::mpsc::Sender<LineMsg>,
) {
    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(Result::ok) {
            if tx.send((source, line)).is_err() {
                break;
            }
        }
    });
}

fn resolve_backend_bin(explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Ok(path) = std::env::var("HYA_BACKEND_BIN") {
        return Ok(PathBuf::from(path));
    }
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for profile in ["release", "debug"] {
        let candidate = workspace.join("target").join(profile).join("hya-backend");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("cannot find hya-backend; build it or pass --backend-bin")
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn wall_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn tempfile_dir() -> anyhow::Result<PathBuf> {
    let base = std::env::temp_dir().join(format!(
        "hya-startup-bench-{}",
        std::process::id()
    ));
    // Unique per call
    let dir = base.join(format!(
        "{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mark_line_accepts_trace_json() {
        let line = r#"{"hya_startup":true,"mark":"backend_listen","wall_ms":1000,"detail":"http://127.0.0.1:9"}"#;
        let mark = parse_mark_line(line).expect("mark");
        assert_eq!(mark.mark, "backend_listen");
        assert_eq!(mark.wall_ms, 1000);
        assert_eq!(mark.detail.as_deref(), Some("http://127.0.0.1:9"));
    }

    #[test]
    fn parse_mark_line_ignores_noise() {
        assert!(parse_mark_line("hya server listening on http://x").is_none());
        assert!(parse_mark_line(r#"{"mark":"x"}"#).is_none());
    }

    #[test]
    fn summarize_marks_deltas_from_t0() {
        let marks = vec![
            StartupMark {
                mark: "a".into(),
                wall_ms: 1000,
                detail: None,
            },
            StartupMark {
                mark: "b".into(),
                wall_ms: 1123,
                detail: None,
            },
        ];
        let summary = summarize_marks(&marks);
        assert_eq!(summary["t0_wall_ms"], 1000);
        assert_eq!(summary["marks"][1]["delta_ms"], 123);
    }
}
