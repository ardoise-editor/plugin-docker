//! The manifest as the editor reads it, and the words of the plugin.

use std::collections::BTreeSet;
use std::path::Path;

fn read(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}

#[test]
fn the_manifest_is_good_and_each_panel_has_a_glyph() {
    let manifest = ardoise_plugin::manifest::Manifest::parse(&read("plugin.json")).unwrap();
    assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
    for panel in &manifest.contributes.panels {
        let icon = panel.icon.as_deref().unwrap_or_default();
        assert!(
            icon.chars().count() == 1
                && ('\u{e000}'..='\u{f8ff}').contains(&icon.chars().next().unwrap()),
            "panel {}: {icon:?} is not one glyph of the Nerd Font",
            panel.id
        );
    }
}

/// The keys written in the sources, those made from the boxes of the cleanup
/// among them.
fn used() -> BTreeSet<String> {
    let manifest: serde_json::Value = serde_json::from_str(&read("plugin.json")).unwrap();
    let commands: Vec<&str> = manifest["contributes"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["id"].as_str())
        .collect();
    let mut files = Vec::new();
    for dir in ["src", "src/ui"] {
        for entry in std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "rs") {
                files.push(std::fs::read_to_string(path).unwrap());
            }
        }
    }
    let mut keys = BTreeSet::new();
    for source in &files {
        for part in source.split("\"docker.").skip(1) {
            let name: String = part
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                .collect();
            // `docker.io` is the host of Docker Hub.
            if name != "io" && part[name.len()..].starts_with('"') {
                let key = format!("docker.{name}");
                // An id of a command is no word unless it is one too.
                if !commands.contains(&key.as_str()) || !["show", "file"].contains(&name.as_str()) {
                    keys.insert(key);
                }
            }
        }
    }
    for id in [
        "stopped",
        "dangling",
        "all_images",
        "networks",
        "anonymous_volumes",
        "all_volumes",
        "cache",
        "all_cache",
    ] {
        keys.insert(format!("docker.clean_{id}"));
    }
    for instruction in [
        "from",
        "run",
        "cmd",
        "copy",
        "add",
        "workdir",
        "env",
        "arg",
        "expose",
        "entrypoint",
        "user",
        "volume",
        "label",
        "healthcheck",
        "shell",
        "stopsignal",
        "onbuild",
        "maintainer",
    ] {
        keys.insert(format!("docker.in_{instruction}"));
    }
    keys
}

#[test]
fn every_word_is_said_in_every_language() {
    let used = used();
    for language in ["en", "fr"] {
        let words: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&read(&format!("locales/{language}.json"))).unwrap();
        let missing: Vec<&String> = used.iter().filter(|k| !words.contains_key(*k)).collect();
        assert!(missing.is_empty(), "{language} lacks {missing:?}");
    }
    let keys = |language: &str| -> BTreeSet<String> {
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&read(&format!(
            "locales/{language}.json"
        )))
        .unwrap()
        .keys()
        .cloned()
        .collect()
    };
    assert_eq!(keys("en"), keys("fr"));
}

/// A picto the editor doesn't know is drawn as its words: only those it has
/// (Ardoise 2.9), or one glyph of the Nerd Font.
#[test]
fn every_picto_is_one_the_editor_draws() {
    const KNOWN: &[&str] = &[
        "archive",
        "check",
        "chevrons-up-down",
        "copy",
        "download",
        "file",
        "file-plus",
        "folder-open",
        "folder-plus",
        "hash",
        "import",
        "list-tree",
        "pencil",
        "play",
        "plus",
        "refresh-cw",
        "rotate-ccw",
        "save",
        "sliders-horizontal",
        "sparkles",
        "square",
        "terminal",
        "trash-2",
        "x",
        "zap",
    ];
    for file in [
        "src/ui/panel.rs",
        "src/ui/detail.rs",
        "src/ui/dialog.rs",
        "src/ui/registry.rs",
    ] {
        let source = read(file);
        for call in ["picto(", "labelled(", "button("] {
            for after in source.split(call).skip(1) {
                let args = &after[..after.find(')').unwrap_or(after.len())];
                let strings: Vec<&str> = args.split('"').skip(1).step_by(2).collect();
                // A picto is the third text of a call: words, action, picto.
                if let [_, _, icon] = strings[..] {
                    assert!(
                        KNOWN.contains(&icon) || icon.starts_with("\\u{f"),
                        "{file}: {icon}"
                    );
                }
            }
        }
    }
}
