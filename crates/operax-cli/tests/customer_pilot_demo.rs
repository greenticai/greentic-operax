use std::process::Command;

#[test]
fn tenancy_batch_dry_run_emits_three_decisions() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root");
    let handoff = workspace.join("examples/tenancy/handoff");
    let input = workspace.join("examples/tenancy/banking/daily-transactions.json");
    let output = Command::new(env!("CARGO_BIN_EXE_greentic-operax"))
        .args([
            "run",
            handoff.to_str().unwrap(),
            "--tenant",
            "demo-tenant",
            "--team",
            "property-ops",
            "--sorx-url",
            "http://localhost:8088",
            "--input",
            input.to_str().unwrap(),
            "--dry-run",
            "--json",
        ])
        .output()
        .expect("run greentic-operax");

    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["input_count"], 3);
    assert_eq!(json["decisions"][0]["outcome"], "matched");
    assert_eq!(json["decisions"][1]["outcome"], "partial_payment");
    assert_eq!(json["decisions"][2]["outcome"], "unmatched");
    assert_eq!(json["applied_actions"], 0);
}
