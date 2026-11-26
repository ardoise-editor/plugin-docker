//! The windows over the editor: what a command needs before it runs.

use ardoise_plugin::ui::*;
use rust_i18n::t;

use super::{error_line, heading, labelled, muted};
use crate::dockerfile::What;
use crate::state::{App, Dialog, DialogKind};

/// A field under its name.
fn field(dialog: &Dialog, id: &str, label: &str, hint: &str) -> Node {
    column([
        muted(t!(label)),
        input(format!("f.{id}"), dialog.form.get_raw(id), hint).on_submit("dlg.ok"),
    ])
    .spacing(2.0)
}

fn tick(dialog: &Dialog, id: &str, label: &str) -> Node {
    checkbox(format!("b.{id}"), t!(label), dialog.form.on(id))
}

fn choice(dialog: &Dialog, id: &str, label: &str, options: Vec<String>) -> Node {
    let picked = dialog.form.get(id);
    column([
        muted(t!(label)),
        select(
            format!("f.{id}"),
            options,
            (!picked.is_empty()).then(|| picked.to_string()),
        ),
    ])
    .spacing(2.0)
}

fn cleanup(dialog: &Dialog) -> Vec<Node> {
    let mut nodes = Vec::new();
    match &dialog.preview {
        Some(preview) => {
            // What it takes and what can be won back come first: the window
            // is not wide.
            let titles = [
                "docker.kind",
                "docker.size",
                "docker.reclaimable",
                "docker.total",
                "docker.active",
            ]
            .map(|k| t!(k).into_owned());
            let rows: Vec<Vec<String>> = preview
                .usage
                .iter()
                .map(|u| {
                    vec![
                        u.kind.clone(),
                        u.size.clone(),
                        u.reclaimable.clone(),
                        u.total_count.clone(),
                        u.active.clone(),
                    ]
                })
                .collect();
            let mut usage = super::table("usage", &titles, &rows);
            for (column, width) in usage
                .columns
                .iter_mut()
                .zip([120.0, 80.0, 130.0, 60.0, 60.0])
            {
                column.width = Some(width);
            }
            nodes.push(Node::from(usage).height(150.0));
        }
        None => nodes.push(muted(t!("docker.measuring"))),
    }
    let mut boxes = Vec::new();
    for id in crate::commands::CLEANUPS {
        boxes.push(tick(dialog, id, &format!("docker.clean_{id}")));
        let items = dialog.preview.as_ref().and_then(|p| p.items.get(id));
        if let Some(items) = items.filter(|_| dialog.form.on(id)) {
            let mut names: Vec<&str> = items.iter().take(8).map(String::as_str).collect();
            if items.len() > names.len() {
                names.push("…");
            }
            boxes.push(muted(if items.is_empty() {
                format!("    {}", t!("docker.nothing"))
            } else {
                format!("    {} : {}", items.len(), names.join(", "))
            }));
        }
    }
    boxes.push(field(dialog, "until", "docker.older_than", "24h"));
    if !dialog.report.is_empty() {
        boxes.push(heading(t!("docker.report")));
        boxes.extend(
            dialog
                .report
                .lines()
                .map(|l| text(l).style(TextStyle::Mono)),
        );
    }
    nodes.push(scroll(column(boxes).spacing(4.0)).height(300.0));
    nodes
}

fn file_actions(app: &App, path: &std::path::Path, line: Option<usize>) -> Vec<Node> {
    let Some(file) = app.file.as_ref().filter(|f| f.path == path) else {
        return vec![muted(t!("docker.gone"))];
    };
    let target = line.and_then(|l| file.targets.iter().find(|t| t.line == l));
    let button = |key: &str, verb: &str, icon: &str| labelled(key, &format!("file.{verb}"), icon);
    let rows: Vec<Vec<Node>> = match file.kind {
        crate::dockerfile::Kind::Compose => {
            let mut rows = vec![
                vec![
                    button("docker.up", "up", "play"),
                    button("docker.up_build", "upbuild", "zap"),
                    button("docker.recreate", "recreate", "rotate-ccw"),
                ],
                vec![
                    button("docker.stop", "stop", "square"),
                    button("docker.restart", "restart", "rotate-ccw"),
                    button("docker.down", "down", "x"),
                ],
                vec![
                    button("docker.logs", "logs", "file"),
                    button("docker.build", "build", "zap"),
                    button("docker.pull", "pull", "download"),
                ],
            ];
            if matches!(target.map(|t| &t.what), Some(What::Service(_))) {
                rows.push(vec![
                    button("docker.shell", "shell", "terminal"),
                    button("docker.scale", "scale", "copy"),
                ]);
            } else {
                rows.push(vec![
                    button("docker.status", "ps", "list-tree"),
                    button("docker.check", "config", "check"),
                    button("docker.down_volumes", "downv", "trash-2"),
                ]);
            }
            rows
        }
        crate::dockerfile::Kind::Dockerfile => vec![
            vec![
                button("docker.build_run", "build_run", "play"),
                button("docker.build", "build", "zap"),
            ],
            vec![
                button(
                    "docker.build_options",
                    "build_options",
                    "sliders-horizontal",
                ),
                button("docker.check", "check", "check"),
            ],
        ],
    };
    rows.into_iter().map(|r| row(r).spacing(6.0)).collect()
}

impl crate::commands::Form {
    /// A field as typed, with its spaces.
    pub fn get_raw(&self, id: &str) -> &str {
        self.fields.get(id).map_or("", String::as_str)
    }
}

pub fn dialog(app: &App, dialog: &Dialog) -> Node {
    let mut end = Vec::new();
    let mut start = Vec::new();
    let mut ok = Some(("docker.ok", ButtonStyle::Primary));
    let mut body: Vec<Node> = match &dialog.kind {
        DialogKind::Run => {
            ok = Some(("docker.run", ButtonStyle::Primary));
            start.push(button(t!("docker.create_only"), "dlg.create").enabled(!dialog.busy));
            let mut networks = vec![String::new()];
            networks.extend(app.data.networks.iter().map(|n| n.name.clone()));
            vec![
                field(dialog, "image", "docker.image", "nginx:latest"),
                field(dialog, "name", "docker.name", ""),
                field(dialog, "ports", "docker.ports", "8080:80 5432:5432"),
                field(
                    dialog,
                    "volumes",
                    "docker.volumes",
                    "data:/var/lib/data \"/my dir:/app\"",
                ),
                field(
                    dialog,
                    "env",
                    "docker.environment",
                    "KEY=value \"OTHER=two words\"",
                ),
                field(dialog, "env_file", "docker.env_file", ".env"),
                row([
                    choice(dialog, "network", "docker.network", networks),
                    choice(
                        dialog,
                        "restart",
                        "docker.restart_policy",
                        ["no", "always", "unless-stopped", "on-failure"]
                            .map(String::from)
                            .to_vec(),
                    ),
                ])
                .spacing(8.0),
                field(dialog, "command", "docker.command", ""),
                field(dialog, "options", "docker.options", "--memory 512m"),
                row([
                    tick(dialog, "detach", "docker.detach"),
                    tick(dialog, "interactive", "docker.interactive"),
                    tick(dialog, "rm", "docker.auto_remove"),
                    tick(dialog, "publish_all", "docker.publish_all"),
                ])
                .spacing(10.0),
            ]
        }
        DialogKind::Pull => {
            ok = Some(("docker.pull", ButtonStyle::Primary));
            vec![field(dialog, "image", "docker.image", "postgres:17")]
        }
        DialogKind::Build => {
            ok = Some(("docker.build", ButtonStyle::Primary));
            start.push(button(t!("docker.build_run"), "dlg.build_run"));
            vec![
                field(dialog, "file", "docker.dockerfile", "Dockerfile"),
                field(dialog, "context", "docker.build_context", "."),
                field(dialog, "tag", "docker.tag", "app:latest"),
                field(dialog, "target", "docker.target", ""),
                field(dialog, "build_args", "docker.build_args", "VERSION=1"),
                field(dialog, "platform", "docker.platform", "linux/amd64"),
                field(dialog, "options", "docker.options", ""),
                row([
                    tick(dialog, "no_cache", "docker.no_cache"),
                    tick(dialog, "pull", "docker.pull_base"),
                ])
                .spacing(10.0),
            ]
        }
        DialogKind::Volume => vec![
            field(dialog, "name", "docker.name", ""),
            field(dialog, "driver", "docker.driver", "local"),
        ],
        DialogKind::Network => vec![
            field(dialog, "name", "docker.name", ""),
            field(dialog, "driver", "docker.driver", "bridge"),
            field(dialog, "subnet", "docker.subnet", "172.30.0.0/16"),
            tick(dialog, "internal", "docker.internal"),
        ],
        DialogKind::Tag(image) => vec![
            muted(image),
            field(dialog, "tag", "docker.new_tag", "registry/name:1.0"),
        ],
        DialogKind::Exec(_) => {
            ok = Some(("docker.run", ButtonStyle::Primary));
            vec![
                field(dialog, "command", "docker.command", "ls -la /"),
                field(dialog, "user", "docker.user", "root"),
                field(dialog, "workdir", "docker.workdir", "/app"),
            ]
        }
        DialogKind::Commit(_) => vec![
            field(dialog, "tag", "docker.new_image", "name:1.0"),
            field(dialog, "message", "docker.message", ""),
        ],
        DialogKind::Scale(compose) => vec![
            muted(compose.service.clone().unwrap_or_default()),
            field(dialog, "count", "docker.replicas", "2"),
        ],
        DialogKind::File(path, line) => {
            ok = None;
            file_actions(app, path, *line)
        }
        DialogKind::Login => {
            ok = Some(("docker.login", ButtonStyle::Primary));
            start.push(button(t!("docker.add_without_login"), "dlg.add").enabled(!dialog.busy));
            vec![
                muted(t!("docker.login_hint")),
                field(
                    dialog,
                    "server",
                    "docker.registry",
                    "docker.io · ghcr.io · registry.gitlab.com",
                ),
                field(dialog, "user", "docker.user", ""),
                column([
                    muted(t!("docker.password")),
                    input("f.password", dialog.form.get_raw("password"), "")
                        .secret()
                        .on_submit("dlg.ok"),
                ])
                .spacing(2.0),
            ]
        }
        DialogKind::Push(image) => {
            ok = Some(("docker.push", ButtonStyle::Primary));
            vec![
                muted(image),
                choice(
                    dialog,
                    "registry",
                    "docker.registry",
                    app.registry_hosts().into_iter().collect(),
                ),
                field(dialog, "repository", "docker.repository", "name/app"),
                field(dialog, "tag", "docker.tag_name", "latest"),
            ]
        }
        DialogKind::Cleanup => {
            ok = Some(("docker.clean_now", ButtonStyle::Danger));
            start.push(button(t!("docker.measure"), "dlg.measure").enabled(!dialog.busy));
            cleanup(dialog)
        }
    };
    body.extend(error_line(&dialog.error));
    if dialog.busy {
        body.push(muted(t!("docker.working")));
    }
    end.push(button(t!("docker.close"), "dlg.cancel"));
    if let Some((key, style)) = ok {
        end.push(
            button(t!(key), "dlg.ok")
                .button_style(style)
                .enabled(!dialog.busy),
        );
    }
    body.push(footer(start, end));
    column(body).spacing(10.0)
}
