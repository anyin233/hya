use std::process::Command;

#[test]
fn agent_list_prints_opencode_native_agent_shape() -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_yaca"))
        .args(["agent", "list"])
        .output()?;

    assert!(
        output.status.success(),
        "agent list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)?,
        concat!(
            "build (primary)\n",
            "  [\n",
            "  {\n",
            "    \"permission\": \"read\",\n",
            "    \"pattern\": \"*\",\n",
            "    \"action\": \"allow\"\n",
            "  },\n",
            "  {\n",
            "    \"permission\": \"glob\",\n",
            "    \"pattern\": \"*\",\n",
            "    \"action\": \"allow\"\n",
            "  },\n",
            "  {\n",
            "    \"permission\": \"grep\",\n",
            "    \"pattern\": \"*\",\n",
            "    \"action\": \"allow\"\n",
            "  }\n",
            "]\n",
        )
    );
    Ok(())
}
