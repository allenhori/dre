use assert_cmd::Command;

#[test]
fn version_flag_prints_unreleased_until_a_release_is_cut() {
    let out = Command::cargo_bin("dre")
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "dre unreleased");
}
