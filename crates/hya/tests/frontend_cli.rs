use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn default_frontend_execs_adjacent_typescript_launcher() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-frontend-delegation")?;
    let project = env.root.join("project");
    let runtime = env.root.join("runtime");
    let bun = env.root.join("bun");
    let bun_args = env.root.join("bun-args");
    let bun_parent = env.root.join("bun-parent");
    std::fs::create_dir(&project)?;
    std::fs::create_dir_all(runtime.join("src"))?;
    std::fs::write(runtime.join("src/main.tsx"), "")?;
    executable(
        &bun,
        "#!/bin/sh\nprintf '%s\n' \"$PPID\" > \"$HYA_TEST_BUN_PARENT\"\nprintf '%s\n' \"$@\" > \"$HYA_TEST_BUN_ARGS\"\nexit 23\n",
    )?;
    assert!(
        PathBuf::from(env!("CARGO_BIN_EXE_hya"))
            .with_file_name("hya-ts")
            .is_file(),
        "build hya-ts before running the hya frontend contract"
    );

    let mut child = hya_command(&env)
        .env("HYA_TUI_TS_DIR", &runtime)
        .env("HYA_TEST_BUN_ARGS", &bun_args)
        .env("HYA_TEST_BUN_PARENT", &bun_parent)
        .arg(&project)
        .arg("--server")
        .arg("http://127.0.0.1:54321")
        .arg("--bun")
        .arg(&bun)
        .arg("--prompt")
        .arg("hello")
        .spawn()?;
    let original_pid = child.id();
    let status = child.wait()?;

    assert_eq!(status.code(), Some(23));
    assert_eq!(
        std::fs::read_to_string(bun_parent)?.trim(),
        original_pid.to_string()
    );
    let args = std::fs::read_to_string(bun_args)?;
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "src/main.tsx",
            "--url",
            "http://127.0.0.1:54321",
            "--project",
            project
                .canonicalize()?
                .to_str()
                .ok_or("project is not UTF-8")?,
            "--prompt",
            "hello",
        ]
    );
    Ok(())
}

#[test]
fn canonical_help_uses_hya_branding() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-frontend-help")?;
    let output = hya_command(&env).arg("--help").output()?;

    assert_success("hya --help", &output);
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Usage: hya "), "{stdout}");
    assert!(!stdout.contains("Usage: hya-ts "), "{stdout}");
    Ok(())
}

#[test]
fn canonical_version_uses_hya_branding() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-frontend-version")?;
    let output = hya_command(&env).arg("--version").output()?;

    assert_success("hya --version", &output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("hya {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    Ok(())
}

#[test]
fn canonical_missing_bun_error_uses_hya_branding() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-frontend-missing-bun")?;
    let runtime = env.root.join("runtime");
    let missing_bun = env.root.join("missing-bun");
    std::fs::create_dir_all(runtime.join("src"))?;
    std::fs::write(runtime.join("src/main.tsx"), "")?;

    let output = hya_command(&env)
        .env("HYA_TUI_TS_DIR", runtime)
        .args(["--server", "http://127.0.0.1:54321", "--bun"])
        .arg(&missing_bun)
        .output()?;

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.starts_with("hya: failed to launch Bun"), "{stderr}");
    assert!(
        stderr.contains(&missing_bun.display().to_string()),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn missing_adjacent_launcher_reports_its_path() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-frontend-missing-launcher")?;
    let relocated = env.root.join("hya");
    std::fs::write(&relocated, std::fs::read(env!("CARGO_BIN_EXE_hya"))?)?;
    std::fs::set_permissions(&relocated, std::fs::Permissions::from_mode(0o755))?;

    let output = Command::new(&relocated).output()?;

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.starts_with("hya: failed to launch"), "{stderr}");
    assert!(
        stderr.contains(&relocated.with_file_name("hya-ts").display().to_string()),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn import_compat_imports_model_config_without_tty() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-frontend-import-compat")?;
    let compat_config = env.root.join("opencode.json");
    let hya_config = env.xdg_config.join("hya/config.yaml");
    std::fs::create_dir_all(hya_config.parent().ok_or("hya config should have parent")?)?;
    std::fs::write(
        &hya_config,
        r#"
default_agent: build
mcp:
  existing_hya_only:
    command: ["python3", "server.py"]
plugins:
  memory:
    command: ["python3", "memory.py"]
"#,
    )?;
    std::fs::write(
        &compat_config,
        r#"{
  "model": "gateway/gpt-5.5",
  "provider": {
    "gateway": {
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "https://gateway.example/v1",
        "apiKey": "{env:GATEWAY_KEY}"
      },
      "models": {
        "gpt-5.5": {},
        "gpt-5.4": {}
      }
    }
  },
  "mcp": {
    "local_tools": {
      "type": "local",
      "command": ["npx", "-y", "@example/mcp"],
      "environment": {
        "TOKEN": "{env:MCP_TOKEN}"
      },
      "enabled": true,
      "timeout": 5000
    },
    "remote_search": {
      "type": "remote",
      "url": "https://mcp.example/sse",
      "enabled": true
    }
  }
}"#,
    )?;

    let output = hya_command(&env)
        .env("COMPAT_CONFIG", &compat_config)
        .args(["--import", "compat"])
        .output()?;

    assert_success("hya --import compat", &output);
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("imported 1 providers and 2 models"),
        "import summary should report model-only Compat import:\n{stdout}"
    );
    assert!(
        stdout.contains("skills import: TODO"),
        "import command should expose the planned skills placeholder:\n{stdout}"
    );
    assert!(
        !stdout.contains("mcp import: TODO"),
        "MCP import should no longer be reported as TODO:\n{stdout}"
    );
    assert!(
        stdout.contains("imported 1 local MCP servers and skipped 1 unsupported MCP entries"),
        "import summary should report local and skipped MCP entries:\n{stdout}"
    );

    let config = std::fs::read_to_string(hya_config)?;
    for expected in [
        "default_model: gateway/gpt-5.5",
        "https://gateway.example/v1",
        "{env:GATEWAY_KEY}",
        "gpt-5.4",
        "gpt-5.5",
        "default_agent: build",
        "existing_hya_only:",
        "memory:",
        "local_tools:",
        "npx",
        "-y",
        "@example/mcp",
        "env:",
        "{env:MCP_TOKEN}",
        "enabled: true",
        "timeout_ms: 5000",
    ] {
        assert!(
            config.contains(expected),
            "written config should contain {expected:?}:\n{config}"
        );
    }
    assert!(
        !config.contains("remote_search") && !config.contains("https://mcp.example/sse"),
        "remote MCP entries should be skipped:\n{config}"
    );
    Ok(())
}

struct IsolatedEnv {
    root: PathBuf,
    home: PathBuf,
    xdg_config: PathBuf,
    path: Option<OsString>,
}

impl IsolatedEnv {
    fn new(prefix: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let root = temp_dir(prefix)?;
        let home = root.join("home");
        let xdg_config = root.join("xdg-config");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&xdg_config)?;
        Ok(Self {
            root,
            home,
            xdg_config,
            path: std::env::var_os("PATH"),
        })
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn hya_command(env: &IsolatedEnv) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
    command.env_clear();
    if let Some(path) = &env.path {
        command.env("PATH", path);
    }
    command
        .env("HOME", &env.home)
        .env("XDG_CONFIG_HOME", &env.xdg_config)
        .env("COMPAT_CONFIG", env.root.join("missing-opencode.json"))
        .env("NO_COLOR", "1");
    command
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn executable(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)?;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)
}

fn temp_dir(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir)?;
    Ok(dir)
}
