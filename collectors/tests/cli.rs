//! CLI contract tests for the risk ingest shim (mirrors the chassis ingest
//! bin: -collector <name> / -list / no-flags → usage + exit 2).

use std::process::Command;

#[test]
fn list_prints_registered_collectors() {
    let out = Command::new(env!("CARGO_BIN_EXE_ingest"))
        .arg("-list")
        .output()
        .expect("run ingest");
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "github\ndepsdev\n");
}

#[test]
fn no_flags_prints_usage_and_exits_2() {
    let out = Command::new(env!("CARGO_BIN_EXE_ingest"))
        .output()
        .expect("run ingest");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("Usage: ingest -collector <name>"));
    assert!(stderr.contains("Registered collectors: [github depsdev]"));
}

#[test]
fn unknown_collector_errors() {
    let out = Command::new(env!("CARGO_BIN_EXE_ingest"))
        .args(["-collector", "nope"])
        .output()
        .expect("run ingest");
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("unknown collector"));
}
