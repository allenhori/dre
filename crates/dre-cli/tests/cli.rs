use assert_cmd::Command;

#[test]
fn version_flag_prints_the_workspace_version() {
    let out = Command::cargo_bin("dre")
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("dre {}", env!("CARGO_PKG_VERSION"))
    );
}
