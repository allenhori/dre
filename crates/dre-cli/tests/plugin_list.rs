mod common;

use assert_cmd::Command;

#[test]
fn plugin_list_shows_kind_name_version_and_protocol() {
    let dir = tempfile::tempdir().unwrap();
    common::place_plugin(dir.path(), "dre-source-fixture");
    // A versioned install, side by side.
    let versioned = dir.path().join("source/fixture/1.2.0");
    std::fs::create_dir_all(&versioned).unwrap();
    std::fs::copy(
        common::workspace_bin("dre-source-fixture"),
        versioned.join(common::workspace_bin("dre-source-fixture").file_name().unwrap()),
    )
    .unwrap();
    // Not a plugin: wrong name shape.
    std::fs::write(dir.path().join("dre-source-fixture.d"), "").unwrap();

    let out = Command::cargo_bin("dre")
        .unwrap()
        .args(["plugin", "list"])
        .env("DRE_PLUGINS_DIR", dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert_eq!(
        lines[0].split_whitespace().collect::<Vec<_>>(),
        ["KIND", "NAME", "VERSION", "PROTOCOL", "PATH"]
    );
    for l in &lines[1..] {
        let cols: Vec<&str> = l.split_whitespace().collect();
        assert_eq!(&cols[..4], &["source", "fixture", "unreleased", "v0"], "{text}");
    }
}

#[test]
fn plugin_list_with_nothing_installed_says_where_it_looked() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("dre")
        .unwrap()
        .args(["plugin", "list"])
        .env("DRE_PLUGINS_DIR", dir.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("No plugins installed in"));
}
