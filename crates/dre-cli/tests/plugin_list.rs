mod common;

use assert_cmd::Command;

#[test]
fn plugin_list_shows_package_provides_version_and_protocol() {
    let dir = tempfile::tempdir().unwrap();
    common::place_plugin(dir.path(), "dre-source-fixture");
    // A versioned install, side by side, with its manifest.
    let versioned = dir.path().join("fixture/1.2.0");
    std::fs::create_dir_all(&versioned).unwrap();
    let exe = common::workspace_bin("dre-source-fixture");
    let name = exe.file_name().unwrap().to_string_lossy().to_string();
    std::fs::copy(&exe, versioned.join(&name)).unwrap();
    std::fs::write(
        versioned.join("plugin.json"),
        serde_json::json!({"executable": name, "provides": ["source/fixture", "destination/inbox"]})
            .to_string(),
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
        ["PACKAGE", "PROVIDES", "VERSION", "PROTOCOL", "PATH"]
    );
    // Flat, named for one plugin; then the installed package.
    let cols = |l: &str| l.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(
        &cols(lines[1])[..4],
        &["fixture", "source/fixture", env!("CARGO_PKG_VERSION"), "v0"],
        "{text}"
    );
    assert_eq!(
        &cols(lines[2])[..5],
        &[
            "fixture",
            "source/fixture,",
            "destination/inbox",
            env!("CARGO_PKG_VERSION"),
            "v0"
        ],
        "{text}"
    );
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
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("No plugin packages installed in"));
}
