use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn frontend_without_tty_creates_config_and_exits_cleanly() -> Result<(), Box<dyn std::error::Error>>
{
    let env = IsolatedEnv::new("hya-frontend-non-tty")?;
    let path = env.xdg_config.join("hya/config.yaml");
    assert!(!path.exists(), "test should start without hya config");

    let output = hya_command(&env).output()?;

    assert_success("hya frontend", &output);
    let config = std::fs::read_to_string(&path)?;
    assert!(
        config.contains("default_model: offline"),
        "created config should contain the offline starter model:\n{config}"
    );
    assert!(
        String::from_utf8(output.stdout)?.contains("needs a terminal"),
        "non-tty frontend run should explain the terminal requirement"
    );
    Ok(())
}

#[test]
fn import_compat_imports_model_config_without_tty() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-frontend-import-compat")?;
    let compat_config = env.root.join("opencode.json");
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
        stdout.contains("mcp import: TODO"),
        "import command should expose the planned MCP placeholder:\n{stdout}"
    );

    let config = std::fs::read_to_string(env.xdg_config.join("hya/config.yaml"))?;
    assert!(config.contains("default_model: \"gateway/gpt-5.5\""));
    assert!(config.contains("base_url: \"https://gateway.example/v1\""));
    assert!(config.contains("api_key: \"{env:GATEWAY_KEY}\""));
    assert!(config.contains("models: [\"gpt-5.4\", \"gpt-5.5\"]"));
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

fn temp_dir(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir)?;
    Ok(dir)
}
