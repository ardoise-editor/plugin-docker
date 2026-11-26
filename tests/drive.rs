//! The plugin as the editor sees it: the program, spoken to on its standard
//! streams, on the Docker engine of this machine (skipped without one).
//!
//! What it makes is named `ardoise-test-…` and removed at the end; nothing
//! else of the engine is touched. The `docker` the plugin finds refuses every
//! `prune`, so that no mistake can clean this machine.

#![cfg(unix)]

use std::io::BufReader;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use ardoise_plugin::protocol::{read_message, write_message, Message};
use serde_json::{json, Value};

const IMAGE: &str = "busybox:stable";
const WAIT: Duration = Duration::from_secs(90);

fn docker(args: &[&str]) -> (bool, String) {
    let out = Command::new("docker").args(args).output().expect("docker");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

fn engine() -> bool {
    Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
        && (docker(&["image", "inspect", IMAGE]).0 || docker(&["pull", IMAGE]).0)
}

/// Removes what the test made, however it ends.
struct Made {
    prefix: String,
    compose: PathBuf,
    /// Volumes and image names made on the way.
    volumes: Vec<String>,
    images: Vec<String>,
}

impl Drop for Made {
    fn drop(&mut self) {
        let project = format!("{}-project", self.prefix);
        let file = self.compose.to_string_lossy().into_owned();
        docker(&["compose", "-p", &project, "-f", &file, "down", "-t", "0"]);
        docker(&["rm", "-f", &format!("{}-box", self.prefix)]);
        docker(&["rm", "-f", &format!("{}-new", self.prefix)]);
        docker(&["volume", "rm", &format!("{}-volume", self.prefix)]);
        docker(&["rm", "-f", "-v", &format!("{}-registry", self.prefix)]);
        for name in &self.volumes {
            docker(&["volume", "rm", name]);
        }
        for image in &self.images {
            docker(&["rmi", image]);
        }
        docker(&["network", "rm", &format!("{}-network", self.prefix)]);
        docker(&["rmi", &format!("{}-image:latest", self.prefix)]);
    }
}

struct Editor {
    child: Child,
    to: ChildStdin,
    from: Receiver<Message>,
    /// What the questions are answered.
    yes: bool,
    /// What the file dialogs of the editor answer.
    picked: Value,
    /// What each panel showed last.
    views: std::collections::HashMap<String, String>,
    /// The answers to what the editor asked.
    answers: std::collections::HashMap<i64, Value>,
    /// The requests and notifications heard, in order.
    heard: Vec<(String, Value)>,
}

impl Editor {
    fn start(data: &Path, bin: &Path) -> Editor {
        // A configuration of its own: logging in and out leaves that of the
        // user as it is.
        let docker_config = data.join("docker-config");
        std::fs::create_dir_all(&docker_config).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_docker-plugin"))
            .env("PATH", path)
            .env("DOCKER_CONFIG", docker_config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let to = child.stdin.take().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let (tx, from) = channel();
        std::thread::spawn(move || {
            while let Ok(Some(message)) = read_message(&mut out) {
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        let mut editor = Editor {
            child,
            to,
            from,
            yes: false,
            picked: Value::Null,
            views: Default::default(),
            heard: Vec::new(),
            answers: Default::default(),
        };
        editor.send(Message::request(
            1,
            "initialize",
            json!({
                "api": 1, "app_version": "2.10.0",
                "settings": {"locale": "en", "dark": true},
                "workspace": [], "plugin_dir": env!("CARGO_MANIFEST_DIR"),
                "data_dir": data,
            }),
        ));
        editor
    }

    fn send(&mut self, message: Message) {
        write_message(&mut self.to, &message).unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(Message::notification(method, params));
    }

    fn press(&mut self, panel: &str, action: &str, value: Value) {
        let mut event = json!({"panel": panel, "action": action});
        if !value.is_null() {
            event["value"] = value;
        }
        self.notify("ui/event", event);
    }

    /// Takes one message in, answering the requests as the editor would.
    /// The labels the plugin proposes at the end of a Dockerfile.
    fn complete(&mut self, id: i64, file: &Path, text: &str) -> Vec<String> {
        self.send(Message::request(
            id,
            "completion/request",
            json!({"panel": "", "editor": file, "text": text, "offset": text.chars().count()}),
        ));
        self.until("the completions", |e| e.answers.contains_key(&id));
        self.answers[&id]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| i["label"].as_str().map(str::to_string))
            .collect()
    }

    fn pump(&mut self, limit: Duration) {
        let Ok(message) = self.from.recv_timeout(limit) else {
            return;
        };
        match message {
            Message::Request { id, method, params } => {
                let answer = match method.as_str() {
                    "window/confirm" => json!(self.yes),
                    "window/pickFile" | "window/pickSavePath" => self.picked.clone(),
                    _ => Value::Null,
                };
                self.heard.push((method, params));
                self.send(Message::Response {
                    id,
                    result: Ok(answer),
                });
            }
            Message::Notification { method, params } => {
                if method == "ui/render" {
                    self.views.insert(
                        params["panel"].as_str().unwrap().to_string(),
                        params["root"].to_string(),
                    );
                } else {
                    self.heard.push((method, params));
                }
            }
            Message::Response { id, result } => {
                self.answers.insert(id, result.unwrap_or_default());
            }
        }
    }

    fn until(&mut self, what: &str, done: impl Fn(&Editor) -> bool) {
        let start = Instant::now();
        while !done(self) {
            assert!(
                start.elapsed() < WAIT,
                "waited for {what}\nviews: {:#?}\nheard: {:#?}",
                self.views,
                self.heard
            );
            self.pump(Duration::from_millis(100));
        }
    }

    fn shows(&mut self, panel: &str, needle: &str) {
        self.until(&format!("{panel} to show {needle}"), |e| {
            e.views.get(panel).is_some_and(|v| v.contains(needle))
        });
    }

    fn hears(&mut self, method: &str, needle: &str) -> Value {
        let found = |e: &Editor| {
            e.heard
                .iter()
                .find(|(m, p)| m == method && p.to_string().contains(needle))
                .map(|(_, p)| p.clone())
        };
        self.until(&format!("{method} with {needle}"), |e| found(e).is_some());
        let params = found(self).unwrap();
        self.heard.clear();
        params
    }

    /// Waits for the engine to be as expected.
    fn engine_until(&mut self, what: &str, done: impl Fn() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < WAIT,
                "waited for {what}\nviews: {:#?}\nheard: {:#?}",
                self.views,
                self.heard
            );
            self.pump(Duration::from_millis(100));
        }
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        let _ = write_message(&mut self.to, &Message::request(99, "shutdown", Value::Null));
        std::thread::sleep(Duration::from_millis(300));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A `docker` that does all but prune.
fn guarded_docker(folder: &Path) {
    let real = Command::new("sh")
        .args(["-c", "command -v docker"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap();
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = prune ] && {{ echo refused >&2; exit 99; }}; done\nexec '{}' \"$@\"\n",
        real.trim()
    );
    let path = folder.join("docker");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn the_plugin_drives_a_real_engine() {
    if !engine() {
        eprintln!("no Docker engine: skipped");
        return;
    }
    let work = tempfile::tempdir().unwrap();
    let prefix = format!("ardoise-test-{}", std::process::id());
    let bin = work.path().join("bin");
    let data = work.path().join("data");
    let project = work.path().join(format!("{prefix}-project"));
    for dir in [&bin, &data, &project] {
        std::fs::create_dir(dir).unwrap();
    }
    guarded_docker(&bin);
    let compose = project.join("compose.yml");
    std::fs::write(
        &compose,
        format!("services:\n  sleeper:\n    image: {IMAGE}\n    command: sleep 600\n    stop_grace_period: 0s\n"),
    )
    .unwrap();
    let dockerfile = project.join("Dockerfile");
    std::fs::write(&dockerfile, format!("FROM {IMAGE}\nCMD [\"true\"]\n")).unwrap();
    let mut made_things = Made {
        prefix: prefix.clone(),
        compose: compose.clone(),
        volumes: Vec::new(),
        images: Vec::new(),
    };

    let name = format!("{prefix}-box");
    let (ok, _) = docker(&[
        "run",
        "-d",
        "--name",
        &name,
        "--init",
        IMAGE,
        "sh",
        "-c",
        "echo hello-from-the-box; sleep 600",
    ]);
    assert!(ok, "a container to look at");
    let (_, id) = docker(&["inspect", "--format", "{{.Id}}", &name]);

    // A helper a plugin killed before could not remove: of a process that
    // is no more. It goes at the first listing.
    let stale = format!("ardoise-volume-999999999-{prefix}");
    let (_, host) = {
        let out = Command::new("uname").arg("-n").output().unwrap();
        (
            true,
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )
    };
    let label = format!("dev.ardoise.helper={host}");
    assert!(
        docker(&[
            "run", "-d", "--rm", "--init", "--name", &stale, "--label", &label, IMAGE, "sleep",
            "300"
        ])
        .0
    );

    let mut editor = Editor::start(&data, &bin);

    // The panel lists the engine.
    editor.notify("panel/opened", json!({"panel": "docker"}));
    editor.shows("docker", &name);
    editor.engine_until("the stale helper to go", || {
        !docker(&["container", "inspect", &stale]).0
    });

    // The logs run in the console, in a real terminal.
    let row = format!("c:{id}");
    editor.press("docker", "ct.logs", json!({"item": row}));
    editor.hears("ui/showPanel", "console");
    editor.notify("panel/opened", json!({"panel": "console"}));
    editor.shows("console", "hello-from-the-box");

    // A shell in the container: what is typed runs in it.
    editor.press("docker", "ct.shell", json!({"item": row}));
    editor.hears("ui/showPanel", "console");
    let typed: Vec<u8> = b"echo shell-$((20+3))\n".to_vec();
    std::thread::sleep(Duration::from_millis(800));
    editor.press(
        "console",
        "screen",
        json!({"kind": "input", "bytes": typed}),
    );
    editor.shows("console", "shell-23");

    // Its tab: information, processes, then files.
    let tab = format!("ct-{}", &id[..12]);
    editor.press("docker", "tree", json!({"item": row, "kind": "click"}));
    editor.hears("editor/openTab", &tab);
    editor.shows(&tab, "sleep 600");
    editor.press(&tab, "detail.tab", json!("top"));
    editor.shows(&tab, "PID");
    editor.press(&tab, "detail.tab", json!("files"));
    editor.shows(&tab, "bin/");
    editor.press(&tab, "fs.go", json!("/tmp"));
    editor.shows(&tab, "\"value\":\"/tmp\"");

    // A folder and a file with an odd name are made in it.
    let inside = name.clone();
    let exists = move |path: &str| docker(&["exec", &inside, "test", "-e", path]).0;
    for (button, typed) in [
        ("fs.new_folder", "a folder"),
        ("fs.new_file", "-x $(no) 'q'"),
    ] {
        editor.press(&tab, button, Value::Null);
        editor.press(&tab, "fs.name", json!(typed));
        editor.press(&tab, "fs.ok", Value::Null);
        editor.engine_until(typed, || exists(&format!("/tmp/{typed}")));
    }
    // Listed: the folder first, then the file.
    editor.shows(&tab, "\"text\":\"-x $(no) 'q'\"");
    editor.shows(&tab, "a folder/");

    // Edited: opened as a copy, written back when it is saved.
    let select = |editor: &mut Editor, row: usize| {
        editor.press(
            &tab,
            "files",
            json!({"kind": "select", "anchor": [row, 0], "cursor": [row, 0]}),
        );
    };
    select(&mut editor, 1);
    editor.press(&tab, "fs.edit", Value::Null);
    let opened = editor.hears("editor/openFile", "-x $(no)");
    let local = PathBuf::from(opened["path"].as_str().unwrap());
    assert!(local.starts_with(&data), "{local:?}");
    std::fs::write(&local, "written in the editor\n").unwrap();
    editor.notify("event/fileSaved", json!({"path": local}));
    editor.hears("window/notify", "Written to");
    let (_, content) = docker(&["exec", &name, "cat", "/tmp/-x $(no) 'q'"]);
    assert_eq!(content, "written in the editor");

    // Renamed, then deleted once the question is answered yes.
    select(&mut editor, 1);
    editor.press(&tab, "fs.rename", Value::Null);
    editor.press(&tab, "fs.name", json!("renamed.txt"));
    editor.press(&tab, "fs.ok", Value::Null);
    editor.engine_until("the rename", || exists("/tmp/renamed.txt"));
    editor.shows(&tab, "\"text\":\"renamed.txt\"");
    select(&mut editor, 1);
    editor.press(&tab, "fs.delete", Value::Null);
    editor.hears("window/confirm", "renamed.txt");
    assert!(exists("/tmp/renamed.txt"), "answered no: still there");
    editor.yes = true;
    select(&mut editor, 1);
    editor.press(&tab, "fs.delete", Value::Null);
    editor.engine_until("the deletion", || !exists("/tmp/renamed.txt"));
    editor.yes = false;

    // Copied from this machine, then back to it.
    let source = work.path().join("sent.txt");
    std::fs::write(&source, "from this machine").unwrap();
    editor.picked = json!(source);
    editor.press(&tab, "fs.upload_file", Value::Null);
    editor.shows(&tab, "\"text\":\"sent.txt\"");
    let back = work.path().join("back.txt");
    editor.picked = json!(back);
    select(&mut editor, 1);
    editor.press(&tab, "fs.download", Value::Null);
    editor.engine_until("the download", || {
        std::fs::read_to_string(&back).is_ok_and(|t| t == "from this machine")
    });
    editor.picked = Value::Null;

    // Statistics, and the image with its layers.
    editor.press(&tab, "detail.tab", json!("stats"));
    editor.shows(&tab, "CPU");
    editor.press("docker", "open", json!({"item": format!("i:{IMAGE}")}));
    let (_, image_id) = docker(&["image", "inspect", "--format", "{{.Id}}", IMAGE]);
    let image_tab = format!("im-{}", &image_id.trim_start_matches("sha256:")[..12]);
    editor.hears("editor/openTab", &image_tab);
    editor.shows(&image_tab, "Platform");
    editor.press(&image_tab, "detail.tab", json!("layers"));
    editor.shows(&image_tab, "\"id\":\"table\"");

    // A command of one's own runs in the console.
    editor.press("docker", "ct.exec", json!({"item": row}));
    editor.hears("editor/openTab", "dlg-exec");
    editor.press("dlg-exec", "f.command", json!("echo exec-$((40+2))"));
    editor.press("dlg-exec", "dlg.ok", Value::Null);
    editor.shows("console", "exec-42");

    // A new container, only created; its dialog says what is wrong first.
    let new = format!("{prefix}-new");
    editor.press("docker", "ct.new", Value::Null);
    editor.hears("editor/openTab", "dlg-run");
    editor.press("dlg-run", "dlg.create", Value::Null);
    editor.shows("dlg-run", "not a valid name");
    editor.press("dlg-run", "f.image", json!(IMAGE));
    editor.press("dlg-run", "f.name", json!(new));
    editor.press("dlg-run", "dlg.create", Value::Null);
    editor.engine_until("the new container", || {
        docker(&["container", "inspect", &new]).0
    });
    // Not running: its files say so. Then it goes.
    editor.shows("docker", &new);
    let (_, new_id) = docker(&["inspect", "--format", "{{.Id}}", &new]);
    let new_row = json!({"item": format!("c:{new_id}")});
    editor.press("docker", "ct.files", new_row.clone());
    editor.shows(&format!("ct-{}", &new_id[..12]), "is not running");
    editor.yes = true;
    editor.press("docker", "ct.remove", new_row);
    editor.engine_until("it to be gone", || {
        !docker(&["container", "inspect", &new]).0
    });
    editor.yes = false;

    // A volume and a network, made by their dialogs and removed from the tree.
    for (what, button, dialog, row, verb) in [
        ("volume", "vo.new", "dlg-volume", "v", "vo.remove"),
        ("network", "ne.new", "dlg-network", "n", "ne.remove"),
    ] {
        let made = format!("{prefix}-{what}");
        editor.press("docker", button, Value::Null);
        editor.hears("editor/openTab", dialog);
        editor.press(dialog, "f.name", json!(made));
        editor.press(dialog, "dlg.ok", Value::Null);
        editor.engine_until("it to be made", || docker(&[what, "inspect", &made]).0);
        editor.yes = true;
        editor.press("docker", verb, json!({"item": format!("{row}:{made}")}));
        editor.engine_until("it to be removed", || !docker(&[what, "inspect", &made]).0);
        editor.yes = false;
    }

    // A compose file in front: marks in the gutter, a click offers to run it.
    editor.notify("event/activeFileChanged", json!({"path": compose}));
    let marks = editor.hears("editor/setRunMarks", "compose.yml");
    assert_eq!(marks["lines"], json!([0, 1]));
    editor.notify("runMark/clicked", json!({"path": compose, "line": 1}));
    editor.hears("editor/openTab", "sleeper");
    editor.shows("dlg-file", "file.up");
    editor.press("dlg-file", "file.up", Value::Null);
    let label = format!("label=com.docker.compose.project={prefix}-project");
    let up = || !docker(&["ps", "-q", "--filter", &label]).1.is_empty();
    editor.engine_until("the service to be up", up);
    let row = format!("p:{prefix}-project");
    editor.shows("docker", &row);
    editor.press("docker", "cp.down", json!({"item": row}));
    editor.engine_until("the project to be down", || !up());

    // A Dockerfile: its stage is marked, and it builds.
    editor.notify("event/activeFileChanged", json!({"path": dockerfile}));
    let marks = editor.hears("editor/setRunMarks", "Dockerfile");
    assert_eq!(marks["lines"], json!([0]));
    editor.press("docker", "file.build", Value::Null);
    let image = format!("{prefix}-project:latest");
    editor.engine_until("the image to be built", || {
        docker(&["image", "inspect", &image]).0
    });
    docker(&["rmi", &image]);

    // The cleanup measures, says what it would run, and does nothing when
    // the answer is no.
    editor.heard.clear();
    editor.notify("command", json!({"command": "docker.cleanup"}));
    editor.shows("dlg-clean", "Build Cache");
    editor.press("dlg-clean", "b.stopped", json!(true));
    editor.press("dlg-clean", "dlg.ok", Value::Null);
    let asked = editor.hears("window/confirm", "docker container prune -f");
    let asked = asked["message"].as_str().unwrap();
    assert!(asked.contains("docker image prune -f"), "{asked}");
    assert!(!asked.contains("volume prune"), "not ticked: {asked}");

    // A Dockerfile is completed: instructions, then the images of the engine.
    editor.notify("event/activeFileChanged", json!({"path": dockerfile}));
    editor.hears("editor/setCompleter", "Dockerfile");
    assert!(editor
        .complete(500, &dockerfile, "FR")
        .contains(&"FROM".to_string()));
    let images = editor.complete(501, &dockerfile, "FROM busy");
    assert!(images.contains(&IMAGE.to_string()), "{images:?}");

    volumes(&mut editor, &prefix, &mut made_things);
    registries(&mut editor, &prefix, &mut made_things, &data);

    // Stopped from the tree.
    editor.press("docker", "ct.stop", json!({"item": format!("c:{id}")}));
    editor.engine_until("the box to stop", || {
        docker(&["inspect", "--format", "{{.State.Running}}", &name]).1 == "false"
    });

    // The editor quits while a volume is looked into: no helper stays.
    let kept = format!("{prefix}-quit");
    assert!(docker(&["volume", "create", &kept]).0);
    made_things.volumes.push(kept.clone());
    editor.press("docker", "refresh", Value::Null);
    editor.shows("docker", &kept);
    editor.press("docker", "open", json!({"item": format!("v:{kept}")}));
    let filter = format!("volume={kept}");
    let helpers = move || docker(&["ps", "-aq", "--filter", &filter]).1;
    editor.engine_until("a helper", || !helpers().is_empty());
    editor.send(Message::request(900, "shutdown", Value::Null));
    editor.engine_until("no helper after quitting", || helpers().is_empty());
    assert!(docker(&["volume", "inspect", &kept]).0);
}

/// What a volume holds is browsed and changed through a helper, which goes
/// with the tab, and never takes the volume with it.
fn volumes(editor: &mut Editor, prefix: &str, made: &mut Made) {
    let named = format!("{prefix}-data");
    assert!(docker(&["volume", "create", &named]).0);
    made.volumes.push(named.clone());
    // One `docker` named itself: `--rm` and `rm -v` take away such volumes
    // when they come with a container, not when they are given by name.
    let (ok, anonymous) = docker(&["volume", "create"]);
    assert!(ok);
    made.volumes.push(anonymous.clone());

    for (volume, id) in [(&named, 600), (&anonymous, 601)] {
        let mount = format!("{volume}:/v");
        assert!(
            docker(&[
                "run",
                "--rm",
                "-v",
                &mount,
                IMAGE,
                "sh",
                "-c",
                "mkdir /v/sub && echo kept > /v/sub/file.txt",
            ])
            .0
        );
        editor.notify("panel/opened", json!({"panel": "docker"}));
        editor.press("docker", "refresh", Value::Null);
        editor.shows("docker", volume);
        let tab = format!("vol-{volume}");
        editor.press(
            "docker",
            "tree",
            json!({"item": format!("v:{volume}"), "kind": "click"}),
        );
        editor.hears("editor/openTab", &tab);
        editor.shows(&tab, "sub/");
        let label = "label=dev.ardoise.helper".to_string();
        let filter = format!("volume={volume}");
        let helpers = move || docker(&["ps", "-aq", "--filter", &label, "--filter", &filter]).1;
        assert!(!helpers().is_empty(), "a helper looks into it");

        let open = |editor: &mut Editor, row: usize| {
            editor.press(&tab, "files", json!({"kind": "open", "row": row, "col": 0}));
        };
        // Into the folder: its path is shown from the top of the volume.
        open(editor, 0);
        editor.shows(&tab, "\"value\":\"/sub\"");
        editor.shows(&tab, "\"text\":\"file.txt\"");
        open(editor, 0);
        let opened = editor.hears("editor/openFile", "file.txt");
        let local = PathBuf::from(opened["path"].as_str().unwrap());
        std::fs::write(&local, format!("changed {id}\n")).unwrap();
        editor.notify("event/fileSaved", json!({"path": local}));
        let told = editor.hears("window/notify", "Written to");
        assert!(told.to_string().contains(":/sub/file.txt"), "{told}");
        // The helper is not listed among the containers.
        editor.press("docker", "refresh", Value::Null);
        std::thread::sleep(Duration::from_millis(800));
        editor.pump(Duration::from_millis(100));
        assert!(!editor.views["docker"].contains("ardoise-volume-"));
        // Nothing goes above the volume.
        editor.press(&tab, "fs.go", json!("../../.."));
        editor.shows(&tab, "\"value\":\"/\"");
        editor.shows(&tab, "sub/");

        editor.notify("tab/closed", json!({"panel": tab}));
        editor.engine_until("the helper to go", || helpers().is_empty());
        assert!(docker(&["volume", "inspect", volume]).0, "{volume} stays");
        let (_, content) = docker(&["run", "--rm", "-v", &mount, IMAGE, "cat", "/v/sub/file.txt"]);
        assert_eq!(content, format!("changed {id}"));

        // Saved once the tab is closed: written, and no helper stays.
        std::fs::write(&local, "after\n").unwrap();
        editor.notify("event/fileSaved", json!({"path": local}));
        editor.hears("window/notify", "Written to");
        editor.engine_until("no helper left", || helpers().is_empty());
    }

    // Removed while it is looked into: the helper goes first.
    let tab = format!("vol-{named}");
    editor.press("docker", "open", json!({"item": format!("v:{named}")}));
    editor.shows(&tab, "sub/");
    editor.yes = true;
    editor.press("docker", "vo.remove", json!({"item": format!("v:{named}")}));
    editor.engine_until("the volume to be removed", || {
        !docker(&["volume", "inspect", &named]).0
    });
    editor.yes = false;
}

/// A registry of this machine: logged in to, pushed to, browsed.
fn registries(editor: &mut Editor, prefix: &str, made: &mut Made, data: &Path) {
    const REGISTRY: &str = "registry:2";
    if !docker(&["image", "inspect", REGISTRY]).0 && !docker(&["pull", REGISTRY]).0 {
        eprintln!("no registry image: skipped");
        return;
    }
    let name = format!("{prefix}-registry");
    assert!(
        docker(&[
            "run",
            "-d",
            "--name",
            &name,
            "-p",
            "127.0.0.1::5000",
            REGISTRY
        ])
        .0
    );
    let (_, bound) = docker(&["port", &name, "5000/tcp"]);
    let host = format!("localhost:{}", bound.rsplit(':').next().unwrap());

    // The password goes to `docker login` and nowhere else.
    editor.press("docker", "rg.add", Value::Null);
    editor.hears("editor/openTab", "dlg-login");
    editor.press("dlg-login", "f.server", json!(host));
    editor.press("dlg-login", "f.user", json!("tester"));
    editor.press("dlg-login", "dlg.ok", Value::Null);
    editor.shows("dlg-login", "Type the password");
    editor.press("dlg-login", "f.password", json!("not-a-real-secret"));
    std::thread::sleep(Duration::from_secs(1));
    editor.press("dlg-login", "dlg.ok", Value::Null);
    editor.hears("window/notify", "Logged in to");
    let config = data.join("docker-config/config.json");
    assert!(std::fs::read_to_string(&config).unwrap().contains(&host));
    for file in ["registries.json", "settings.json", "edits.json"] {
        let kept = std::fs::read_to_string(data.join(file)).unwrap_or_default();
        assert!(!kept.contains("not-a-real-secret"), "{file}");
    }

    // An image sent to it from the tree.
    editor.press("docker", "im.push", json!({"item": format!("i:{IMAGE}")}));
    editor.hears("editor/openTab", "dlg-push");
    editor.press("dlg-push", "f.registry", json!(host));
    editor.press("dlg-push", "f.repository", json!(format!("{prefix}/box")));
    editor.press("dlg-push", "f.tag", json!("one"));
    editor.press("dlg-push", "dlg.ok", Value::Null);
    let sent = format!("{host}/{prefix}/box:one");
    made.images.push(sent.clone());
    editor.shows("console", "digest: sha256");

    // Its repositories, then the tags of one.
    let tab = format!("reg-{}", host.replace(':', "_"));
    editor.press("docker", "open", json!({"item": format!("r:{host}")}));
    editor.hears("editor/openTab", &tab);
    editor.shows(&tab, &format!("{prefix}/box"));
    editor.press(
        &tab,
        "repositories",
        json!({"kind": "open", "row": 0, "col": 0}),
    );
    editor.shows(&tab, "\"text\":\"one\"");
    editor.press(
        &tab,
        "tags",
        json!({"kind": "select", "anchor": [0, 0], "cursor": [0, 0]}),
    );
    docker(&["rmi", &sent]);
    editor.press(&tab, "reg.pull", Value::Null);
    editor.engine_until("it to be pulled back", || {
        docker(&["image", "inspect", &sent]).0
    });
    editor.press(&tab, "reg.typed", json!("no/such"));
    editor.press(&tab, "reg.tags", Value::Null);
    editor.shows(&tab, "Not found on this registry");

    editor.yes = true;
    editor.press(&tab, "rg.logout", Value::Null);
    editor.engine_until("to be logged out", || {
        !std::fs::read_to_string(&config).unwrap().contains(&host)
    });
    editor.yes = false;
}
