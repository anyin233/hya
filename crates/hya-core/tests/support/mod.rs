#![allow(dead_code, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use hya_tool::{Tool, ToolCtx, ToolError};
use serde_json::{Value, json};

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new(label: &str) -> Self {
        let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "hya-runtime-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create test workdir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_skill(&self, name: &str) {
        let dir = self.path.join(".hya/skills").join(name);
        std::fs::create_dir_all(&dir).expect("create skill directory");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} skill\n---\n{name} body\n"),
        )
        .expect("write skill");
    }

    pub fn remove_skill(&self, name: &str) {
        std::fs::remove_dir_all(self.path.join(".hya/skills").join(name))
            .expect("remove skill directory");
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub struct MarkerTool {
    name: String,
}

impl MarkerTool {
    pub fn new(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { name: name.into() })
    }
}

#[async_trait]
impl Tool for MarkerTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name.clone()),
            description: format!("{} marker", self.name),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}
