//! The tab of a container, an image, a volume or a network.

use ardoise_plugin::ui::*;
use rust_i18n::t;
use serde_json::Value;

use super::{error_line, heading, labelled, muted, picto, table};
use crate::engine::short;
use crate::state::{App, Asking, ContainerView, FilesView, ImageView, ObjectKind, ObjectView};

fn str_at<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

/// The words of a list of the inspect (`Cmd`, `Entrypoint`).
fn joined(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// A title and its lines, when there are some.
fn section(key: &str, lines: Vec<String>) -> Vec<Node> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut nodes = vec![space(4.0), heading(t!(key))];
    nodes.extend(lines.into_iter().map(|l| text(l).style(TextStyle::Mono)));
    nodes
}

fn fact(key: &str, value: impl Into<String>) -> Option<Node> {
    let value = value.into();
    (!value.is_empty()).then(|| text(format!("{} : {value}", t!(key))))
}

fn pairs(value: &Value, pointer: &str) -> Vec<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default()
}

fn strings(value: &Value, pointer: &str) -> Vec<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn container_info(inspect: &Value) -> Node {
    let mut nodes: Vec<Node> = [
        fact("docker.image", str_at(inspect, "/Config/Image")),
        fact("docker.status", str_at(inspect, "/State/Status")),
        fact("docker.health", str_at(inspect, "/State/Health/Status")),
        fact("docker.created", str_at(inspect, "/Created")),
        fact("docker.entrypoint", joined(inspect, "/Config/Entrypoint")),
        fact("docker.command", joined(inspect, "/Config/Cmd")),
        fact("docker.workdir", str_at(inspect, "/Config/WorkingDir")),
        fact("docker.user", str_at(inspect, "/Config/User")),
        fact(
            "docker.restart_policy",
            str_at(inspect, "/HostConfig/RestartPolicy/Name"),
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    let ports = inspect
        .pointer("/NetworkSettings/Ports")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(inside, outside)| {
                    let outside: Vec<String> = outside
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|b| format!("{}:{}", str_at(b, "/HostIp"), str_at(b, "/HostPort")))
                        .collect();
                    if outside.is_empty() {
                        inside.clone()
                    } else {
                        format!("{} → {inside}", outside.join(", "))
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    nodes.extend(section("docker.ports", ports));
    let networks = inspect
        .pointer("/NetworkSettings/Networks")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(name, n)| format!("{name}  {}", str_at(n, "/IPAddress")))
                .collect()
        })
        .unwrap_or_default();
    nodes.extend(section("docker.networks", networks));
    let mounts = inspect
        .pointer("/Mounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|m| {
            let from = match str_at(m, "/Name") {
                "" => str_at(m, "/Source"),
                name => name,
            };
            format!("{from} → {}", str_at(m, "/Destination"))
        })
        .collect();
    nodes.extend(section("docker.volumes", mounts));
    nodes.extend(section(
        "docker.environment",
        strings(inspect, "/Config/Env"),
    ));
    nodes.extend(section("docker.labels", pairs(inspect, "/Config/Labels")));
    scroll(column(nodes).spacing(2.0))
}

fn json_view(id: &str, value: &Value) -> Node {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
    editor(id, pretty, "json").read_only()
}

fn size_text(bytes: u64) -> String {
    let mut size = bytes as f64;
    for unit in ["B", "kB", "MB", "GB"] {
        if size < 1000.0 {
            return if unit == "B" {
                format!("{bytes} B")
            } else {
                format!("{size:.1} {unit}")
            };
        }
        size /= 1000.0;
    }
    format!("{size:.1} TB")
}

/// The files of a container, or of a volume (whose helper runs as root).
fn files(view: &FilesView, running: bool, volume: bool) -> Node {
    let mut path_row = vec![
        picto("docker.parent_folder", "fs.up", "\u{f062}"),
        picto("docker.refresh", "fs.refresh", "refresh-cw"),
        input(
            "fs.path",
            view.typed.as_ref().unwrap_or(&view.path),
            t!("docker.path"),
        )
        .on_submit("fs.go"),
    ];
    if !volume {
        path_row.push(checkbox("fs.root", t!("docker.as_root"), view.root));
    }
    let mut nodes = vec![row(path_row).spacing(4.0)];
    let picked = view.selection.is_some() && !view.entries.is_empty();
    let on = |node: Node, yes: bool| node.enabled(yes);
    nodes.push(
        row([
            on(
                labelled("docker.new_file", "fs.new_file", "file-plus"),
                running,
            ),
            on(
                labelled("docker.new_folder", "fs.new_folder", "folder-plus"),
                running,
            ),
            labelled("docker.upload_file", "fs.upload_file", "import"),
            labelled("docker.upload_path", "fs.upload_path", "folder-open"),
            fill(),
            on(picto("docker.edit", "fs.edit", "pencil"), picked && running),
            on(picto("docker.download", "fs.download", "download"), picked),
            on(
                picto("docker.rename", "fs.rename", "\u{f246}"),
                picked && running,
            ),
            on(
                picto("docker.delete", "fs.delete", "trash-2"),
                picked && running,
            ),
        ])
        .spacing(4.0),
    );
    if let Some((asking, value)) = &view.asking {
        let label = match asking {
            Asking::File => t!("docker.ask_file"),
            Asking::Folder => t!("docker.ask_folder"),
            Asking::Rename(old) => t!("docker.ask_rename", name = old),
            Asking::Upload => t!("docker.ask_upload"),
        };
        nodes.push(
            row([
                text(label),
                input("fs.name", value, "").on_submit("fs.ok"),
                button(t!("docker.ok"), "fs.ok").button_style(ButtonStyle::Primary),
                button(t!("docker.cancel"), "fs.cancel"),
            ])
            .spacing(4.0),
        );
    }
    nodes.extend(error_line(&view.error));
    if view.loading {
        nodes.push(muted(t!("docker.loading")));
    } else if !running {
        nodes.push(muted(t!("docker.files_stopped")));
    } else {
        let titles = [
            t!("docker.name").into_owned(),
            t!("docker.size").into_owned(),
            t!("docker.rights").into_owned(),
            t!("docker.owner").into_owned(),
        ];
        let rows: Vec<Vec<String>> = view
            .entries
            .iter()
            .map(|e| {
                let mark = match (e.folder, e.link) {
                    (true, _) => "/",
                    (false, true) => " ↪",
                    _ => "",
                };
                vec![
                    format!("{}{mark}", e.name),
                    if e.folder {
                        String::new()
                    } else {
                        size_text(e.size)
                    },
                    e.mode.clone(),
                    e.owner.clone(),
                ]
            })
            .collect();
        let mut listing = table("files", &titles, &rows);
        listing.selection = view.selection;
        listing.menu = vec![
            MenuItem::new(t!("docker.open_entry"), "fs.open").key("Enter"),
            MenuItem::new(t!("docker.download"), "fs.download"),
            MenuItem::new(t!("docker.rename"), "fs.rename").key("F2"),
            MenuItem::new(t!("docker.copy_path"), "fs.copy_path"),
            MenuItem::new(t!("docker.delete"), "fs.delete"),
        ];
        nodes.push(listing.into());
    }
    column(nodes).spacing(6.0)
}

pub fn container(app: &App, view: &ContainerView) -> Node {
    let listed = app.data.container(&view.id);
    let running = listed.is_some_and(|c| c.running());
    let paused = listed.is_some_and(|c| c.paused());
    let mut nodes = vec![tabs(
        "detail.tab",
        [
            item("info", t!("docker.information")),
            item("files", t!("docker.files")),
            item("top", t!("docker.processes")),
            item("stats", t!("docker.statistics")),
            item("inspect", t!("docker.inspect")),
        ],
    )
    .selected(&view.tab)];
    let Some(listed) = listed else {
        nodes.push(muted(t!("docker.gone")));
        return column(nodes).spacing(8.0).padding(10.0);
    };
    nodes.push(
        row([
            heading(&listed.name),
            muted(format!("{} · {}", short(&listed.id), listed.status)),
        ])
        .spacing(8.0),
    );
    let mut actions = Vec::new();
    if running {
        actions.extend([
            labelled("docker.stop", "ct.stop", "square"),
            labelled("docker.restart", "ct.restart", "rotate-ccw"),
            labelled("docker.logs", "ct.logs", "file"),
            labelled("docker.shell", "ct.shell", "terminal"),
            labelled("docker.exec", "ct.exec", "chevrons-up-down"),
            picto("docker.pause", "ct.pause", "\u{f04c}"),
            picto("docker.kill", "ct.kill", "\u{f05e}"),
        ]);
    } else if paused {
        actions.extend([
            labelled("docker.unpause", "ct.unpause", "play"),
            labelled("docker.logs", "ct.logs", "file"),
        ]);
    } else {
        actions.extend([
            labelled("docker.start", "ct.start", "play"),
            labelled("docker.logs", "ct.logs", "file"),
        ]);
    }
    actions.extend([fill(), picto("docker.remove", "ct.remove", "trash-2")]);
    nodes.push(row(actions).spacing(4.0));
    nodes.extend(error_line(&view.error));
    nodes.push(match view.tab.as_str() {
        "files" => files(&view.files, running, false),
        "top" | "stats" => match &view.table {
            Some((titles, rows)) => column([
                row([picto("docker.refresh", "detail.refresh", "refresh-cw")]),
                table("table", titles, rows).into(),
            ]),
            None if running => muted(t!("docker.loading")),
            None => muted(t!("docker.not_running")),
        },
        tab => match &view.inspect {
            Some(inspect) if tab == "inspect" => json_view("inspect", inspect),
            Some(inspect) => container_info(inspect),
            None => muted(t!("docker.loading")),
        },
    });
    column(nodes).spacing(8.0).padding(10.0)
}

pub fn image(app: &App, view: &ImageView) -> Node {
    let mut nodes = vec![tabs(
        "detail.tab",
        [
            item("info", t!("docker.information")),
            item("layers", t!("docker.layers")),
            item("inspect", t!("docker.inspect")),
        ],
    )
    .selected(&view.tab)];
    let names: Vec<String> = app
        .data
        .images
        .iter()
        .filter(|i| short(&i.id) == short(&view.id))
        .map(|i| i.reference())
        .collect();
    if names.is_empty() {
        nodes.push(muted(t!("docker.gone")));
        return column(nodes).spacing(8.0).padding(10.0);
    }
    nodes.push(heading(names.join(", ")));
    nodes.push(
        row([
            labelled("docker.run_image", "im.run", "play"),
            labelled("docker.tag", "im.tag", "hash"),
            labelled("docker.push", "im.push", "import"),
            labelled("docker.save_image", "im.save", "save"),
            fill(),
            picto("docker.remove", "im.remove", "trash-2"),
        ])
        .spacing(4.0),
    );
    nodes.extend(error_line(&view.error));
    nodes.push(match view.tab.as_str() {
        "layers" => match &view.table {
            Some((titles, rows)) => table("table", titles, rows).into(),
            None => muted(t!("docker.loading")),
        },
        tab => match &view.inspect {
            Some(inspect) if tab == "inspect" => json_view("inspect", inspect),
            Some(inspect) => {
                let size = inspect.pointer("/Size").and_then(Value::as_u64);
                let mut info: Vec<Node> = [
                    fact("docker.created", str_at(inspect, "/Created")),
                    fact("docker.size", size.map(size_text).unwrap_or_default()),
                    fact(
                        "docker.platform",
                        format!(
                            "{}/{}",
                            str_at(inspect, "/Os"),
                            str_at(inspect, "/Architecture")
                        ),
                    ),
                    fact("docker.entrypoint", joined(inspect, "/Config/Entrypoint")),
                    fact("docker.command", joined(inspect, "/Config/Cmd")),
                    fact("docker.workdir", str_at(inspect, "/Config/WorkingDir")),
                    fact("docker.user", str_at(inspect, "/Config/User")),
                ]
                .into_iter()
                .flatten()
                .collect();
                let ports = inspect
                    .pointer("/Config/ExposedPorts")
                    .and_then(Value::as_object)
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                info.extend(section("docker.ports", ports));
                info.extend(section(
                    "docker.environment",
                    strings(inspect, "/Config/Env"),
                ));
                info.extend(section("docker.labels", pairs(inspect, "/Config/Labels")));
                scroll(column(info).spacing(2.0))
            }
            None => muted(t!("docker.loading")),
        },
    });
    column(nodes).spacing(8.0).padding(10.0)
}

/// A volume or a network: its inspect, and what can be done to it.
pub fn object(app: &App, view: &ObjectView) -> Node {
    let mut nodes = vec![heading(&view.name)];
    match view.kind {
        ObjectKind::Volume => {
            nodes.insert(
                0,
                tabs(
                    "detail.tab",
                    [
                        item("files", t!("docker.files")),
                        item("inspect", t!("docker.inspect")),
                    ],
                )
                .selected(&view.tab),
            );
            nodes.push(row([labelled("docker.remove", "vo.remove", "trash-2")]));
            if view.tab == "files" {
                nodes.push(muted(t!("docker.volume_helper")));
                nodes.extend(error_line(&view.error));
                nodes.push(files(&view.files, true, true));
                return column(nodes).spacing(8.0).padding(10.0);
            }
        }
        ObjectKind::Network => {
            let builtin = app
                .data
                .networks
                .iter()
                .any(|n| n.name == view.name && n.builtin());
            nodes.push(
                row([
                    select(
                        "ne.pick",
                        app.data.containers.iter().map(|c| c.name.clone()),
                        view.picked.clone(),
                    )
                    .placeholder(t!("docker.pick_container")),
                    button(t!("docker.connect"), "ne.connect").enabled(view.picked.is_some()),
                    button(t!("docker.disconnect"), "ne.disconnect").enabled(view.picked.is_some()),
                    fill(),
                    labelled("docker.remove", "ne.remove", "trash-2").enabled(!builtin),
                ])
                .spacing(6.0),
            );
        }
    }
    nodes.extend(error_line(&view.error));
    nodes.push(match &view.inspect {
        Some(inspect) => json_view("inspect", inspect),
        None => muted(t!("docker.loading")),
    });
    column(nodes).spacing(8.0).padding(10.0)
}
