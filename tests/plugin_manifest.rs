use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path)).unwrap()
}

#[test]
fn manifest_identifies_the_preview_plugin_and_enumerates_its_stable_actions() {
    let manifest: toml::Value = toml::from_str(&read("herdr-plugin.toml")).unwrap();

    assert_eq!(manifest["id"].as_str(), Some("pi-dal.herdr-preview"));
    assert_eq!(manifest["name"].as_str(), Some("Herdr Preview"));
    assert_eq!(manifest["version"].as_str(), Some("0.1.0"));
    assert_eq!(manifest["min_herdr_version"].as_str(), Some("0.8.0"));
    assert_eq!(
        manifest["description"].as_str(),
        Some(
            "A diff-first review pane for previewing agent changes and sending line comments back through Herdr."
        )
    );

    let panes = manifest["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0]["id"].as_str(), Some("pane"));
    assert_eq!(panes[0]["title"].as_str(), Some("Herdr Preview"));
    assert_eq!(panes[0]["placement"].as_str(), Some("split"));
    assert_eq!(
        panes[0]["command"].as_array().unwrap(),
        &[
            toml::Value::String("sh".into()),
            toml::Value::String("-c".into()),
            toml::Value::String("exec \"$HERDR_PLUGIN_ROOT/bin/herdr-preview\"".into()),
        ]
    );

    let actions = manifest["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 18);
    let expected: [(&str, &str, &[&str]); 18] = [
        ("toggle", "Herdr Preview: toggle preview", &["toggle"]),
        ("open", "Herdr Preview: open preview", &["open"]),
        ("close", "Herdr Preview: close preview", &["close"]),
        ("peek", "Herdr Preview: peek beside agent", &["peek"]),
        ("changes", "Herdr Preview: show changes", &["forward", "alt+d"]),
        ("files", "Herdr Preview: show files", &["forward", "alt+f"]),
        ("review", "Herdr Preview: show review", &["forward", "alt+r"]),
        ("comment", "Herdr Preview: comment", &["forward", "alt+c"]),
        ("comments", "Herdr Preview: list comments", &["forward", "alt+l"]),
        ("send", "Herdr Preview: send comments", &["forward", "alt+s"]),
        ("refresh", "Herdr Preview: refresh", &["forward", "alt+shift+r"]),
        ("hide-unchanged", "Herdr Preview: hide unchanged", &["forward", "alt+u"]),
        ("previous-change", "Herdr Preview: previous change", &["forward", "alt+up"]),
        ("next-change", "Herdr Preview: next change", &["forward", "alt+down"]),
        ("previous-file", "Herdr Preview: previous file", &["forward", "alt+shift+up"]),
        ("next-file", "Herdr Preview: next file", &["forward", "alt+shift+down"]),
        ("previous-change-run", "Herdr Preview: previous change run", &["forward", "alt+left"]),
        ("next-change-run", "Herdr Preview: next change run", &["forward", "alt+right"]),
    ];
    for (action, (id, title, args)) in actions.iter().zip(expected) {
        assert_eq!(action["id"].as_str(), Some(id));
        assert_eq!(action["title"].as_str(), Some(title));
        assert_eq!(
            action["contexts"].as_array().unwrap(),
            &[toml::Value::String("pane".into()), toml::Value::String("workspace".into()),]
        );
        let command = action["command"].as_array().unwrap();
        assert_eq!(command[0].as_str(), Some("bash"));
        assert_eq!(command[1].as_str(), Some("herdr/pane.sh"));
        assert_eq!(command.len(), 2 + args.len());
        for (argument, expected) in command[2..].iter().zip(args) {
            assert_eq!(argument.as_str(), Some(*expected));
        }
    }

    let events = manifest["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["on"].as_str(), Some("worktree.created"));
    assert_eq!(events[0]["command"][2].as_str(), Some("auto-open"));
}

#[test]
fn manifest_versions_and_fork_metadata_stay_consistent() {
    let manifest: toml::Value = toml::from_str(&read("herdr-plugin.toml")).unwrap();
    let cargo: toml::Value = toml::from_str(&read("Cargo.toml")).unwrap();
    let lock: toml::Value = toml::from_str(&read("Cargo.lock")).unwrap();

    let version = manifest["version"].as_str().unwrap();
    assert_eq!(cargo["package"]["version"].as_str(), Some(version));
    assert_eq!(cargo["bin"][0]["name"].as_str(), Some("herdr-preview"));
    assert_eq!(cargo["bin"][0]["path"].as_str(), Some("src/main.rs"));
    assert_eq!(
        cargo["package"]["repository"].as_str(),
        Some("https://github.com/pi-dal/herdr-preview")
    );
    assert_eq!(
        cargo["package"]["homepage"].as_str(),
        Some("https://github.com/pi-dal/herdr-preview")
    );
    let package = lock["package"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"].as_str() == Some("herdr-reviewr"))
        .unwrap();
    assert_eq!(package["version"].as_str(), Some(version));
}

#[test]
fn managed_install_builds_this_source_and_uses_the_fresh_inode_swap() {
    let manifest: toml::Value = toml::from_str(&read("herdr-plugin.toml")).unwrap();
    assert_eq!(manifest["build"][0]["command"][0].as_str(), Some("bash"));
    assert_eq!(manifest["build"][0]["command"][1].as_str(), Some("herdr/build.sh"));

    let script = read("herdr/build.sh");
    assert!(script.contains("cargo build --release"));
    assert!(script.contains("scripts/swap-binary.sh"));
    assert!(script.contains("pi-dal.herdr-preview"));
    assert!(!script.contains("persiyanov/herdr-reviewr"));
    assert!(!script.contains("releases/download"));
    assert!(!root().join("herdr/install.sh").exists());
}

#[test]
fn active_docs_and_runtime_paths_use_the_preview_identity() {
    let pane = read("herdr/pane.sh");
    let config = read("specs/config.md");
    let host = read("specs/herdr-host.md");
    let readme = read("README.md");

    for text in [&pane, &config, &host, &readme] {
        assert!(text.contains("pi-dal.herdr-preview"));
        assert!(!text.contains("persiyanov.reviewr"));
    }
    for text in [&pane, &host] {
        assert!(text.contains(".local/state/herdr/plugins/pi-dal.herdr-preview/bin"));
        assert!(text.contains("Preview"));
    }
    assert!(
        readme.contains("pi-dal.herdr-preview.peek")
            || readme.contains("invoke peek --plugin pi-dal.herdr-preview")
    );
    assert!(readme.contains("no Pi extension"));
    assert!(readme.contains("persiyanov/herdr-reviewr"), "README must retain upstream attribution");
    assert!(pane.contains("== \"herdr-preview\""));
    assert!(host.contains("`herdr-reviewr` belongs to the upstream plugin"));
}
