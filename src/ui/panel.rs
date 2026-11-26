//! The panel on the left (the tree of what the engine holds, as the
//! Services window of an IDE) and the console at the bottom.

use ardoise_plugin::ui::*;
use rust_i18n::t;

use super::{error_line, heading, labelled, muted, picto};
use crate::dockerfile::{Kind, What};
use crate::engine::{short, Container};
use crate::state::App;

fn menu(key: &str, action: &str) -> MenuItem {
    MenuItem::new(t!(key), action)
}

/// What can be done to a container, as it stands.
fn container_menu(c: &Container) -> Vec<MenuItem> {
    let (running, paused) = (c.running(), c.paused());
    let mut items = vec![menu("docker.details", "open")];
    if running {
        items.extend([
            menu("docker.logs", "ct.logs"),
            menu("docker.shell", "ct.shell"),
            menu("docker.shell_root", "ct.shell_root"),
            menu("docker.exec", "ct.exec"),
            menu("docker.attach", "ct.attach"),
            menu("docker.files", "ct.files"),
            menu("docker.stop", "ct.stop"),
            menu("docker.restart", "ct.restart"),
            menu("docker.pause", "ct.pause"),
            menu("docker.kill", "ct.kill"),
        ]);
    } else if paused {
        items.extend([
            menu("docker.logs", "ct.logs"),
            menu("docker.unpause", "ct.unpause"),
            menu("docker.stop", "ct.stop"),
        ]);
    } else {
        items.extend([
            menu("docker.start", "ct.start"),
            menu("docker.logs", "ct.logs"),
            menu("docker.files", "ct.files"),
        ]);
    }
    items.extend([
        menu("docker.commit", "ct.commit"),
        menu("docker.copy_id", "ct.copy_id"),
        menu("docker.remove", "ct.remove"),
    ]);
    items
}

fn compose_menu(whole: bool) -> Vec<MenuItem> {
    let mut items = vec![
        menu("docker.up", "cp.up"),
        menu("docker.up_build", "cp.upbuild"),
        menu("docker.recreate", "cp.recreate"),
        menu("docker.start", "cp.start"),
        menu("docker.stop", "cp.stop"),
        menu("docker.restart", "cp.restart"),
        menu("docker.logs", "cp.logs"),
        menu("docker.pull", "cp.pull"),
        menu("docker.build", "cp.build"),
    ];
    if whole {
        items.extend([
            menu("docker.open_file", "cp.open_file"),
            menu("docker.down", "cp.down"),
            menu("docker.down_volumes", "cp.downv"),
            menu("docker.down_all", "cp.downall"),
        ]);
    } else {
        items.extend([
            menu("docker.scale", "cp.scale"),
            menu("docker.down", "cp.down"),
        ]);
    }
    items
}

fn container_row(c: &Container, label: &str) -> TreeItem {
    let icon = if c.running() {
        "●"
    } else if c.paused() {
        "‖"
    } else {
        "○"
    };
    tree_item(format!("c:{}", c.id), label)
        .icon(icon)
        // The panel is narrow: the status and the ports are in its tab.
        .detail(c.image.clone())
        .menu(container_menu(c))
}

/// The tree of what the engine holds, cut down by the filter.
fn engine_tree(app: &App) -> Node {
    let filter = app.filter.trim().to_lowercase();
    let matches = |text: &str| filter.is_empty() || text.to_lowercase().contains(&filter);
    let open = |id: &str| !filter.is_empty() || app.open.contains(id);
    let group = |id: &str, key: &str, count: String, children: Vec<TreeItem>| {
        tree_item(id, t!(key))
            .detail(count)
            .expandable()
            .expanded(open(id))
            .children(children)
    };

    let projects: Vec<TreeItem> = app
        .data
        .projects()
        .into_iter()
        .filter_map(|(name, containers)| {
            let whole = matches(name);
            let running = containers.iter().filter(|c| c.running()).count();
            let total = containers.len();
            let mut services: std::collections::BTreeMap<&str, Vec<&Container>> =
                Default::default();
            for c in containers {
                if whole || matches(&c.name) || matches(&c.service) || matches(&c.image) {
                    services.entry(&c.service).or_default().push(c);
                }
            }
            if services.is_empty() {
                return None;
            }
            let id = format!("p:{name}");
            let rows = services.into_iter().map(|(service, containers)| {
                let id = format!("s:{name}/{service}");
                let up = containers.iter().filter(|c| c.running()).count();
                tree_item(&id, service)
                    .icon(if up > 0 { "●" } else { "○" })
                    .detail(format!("{up}/{}", containers.len()))
                    .expandable()
                    .expanded(open(&id))
                    .menu(compose_menu(false))
                    .children(containers.iter().map(|c| container_row(c, &c.name)))
            });
            Some(
                tree_item(&id, name)
                    .icon(if running > 0 { "●" } else { "○" })
                    .detail(format!("{running}/{total}"))
                    .expandable()
                    .expanded(open(&id))
                    .menu(compose_menu(true))
                    .children(rows),
            )
        })
        .collect();

    let alone: Vec<&Container> = app
        .data
        .containers
        .iter()
        .filter(|c| c.project.is_empty())
        .collect();
    let alone_running = alone.iter().filter(|c| c.running()).count();
    let alone_total = alone.len();
    let containers: Vec<TreeItem> = alone
        .into_iter()
        .filter(|c| matches(&c.name) || matches(&c.image))
        .map(|c| container_row(c, &c.name))
        .collect();

    let images: Vec<TreeItem> = app
        .data
        .images
        .iter()
        .filter(|i| matches(&i.reference()) || matches(&i.repository))
        .map(|i| {
            let label = if i.dangling() {
                format!("<none> {}", short(&i.id))
            } else {
                i.reference()
            };
            tree_item(format!("i:{}", i.reference()), label)
                .detail(format!("{} · {}", i.size, i.created_since))
                .menu([
                    menu("docker.details", "open"),
                    menu("docker.run_image", "im.run"),
                    menu("docker.layers", "im.layers"),
                    menu("docker.pull", "im.pull").enabled(!i.dangling()),
                    menu("docker.push", "im.push").enabled(!i.dangling()),
                    menu("docker.tag", "im.tag"),
                    menu("docker.save_image", "im.save"),
                    menu("docker.copy_id", "im.copy_id"),
                    menu("docker.remove", "im.remove"),
                    menu("docker.force_remove", "im.force_remove"),
                ])
        })
        .collect();

    let volumes: Vec<TreeItem> = app
        .data
        .volumes
        .iter()
        .filter(|v| matches(&v.name))
        .map(|v| {
            tree_item(format!("v:{}", v.name), &v.name)
                .detail(v.driver.clone())
                .menu([
                    menu("docker.browse", "open"),
                    menu("docker.remove", "vo.remove"),
                ])
        })
        .collect();

    let networks: Vec<TreeItem> = app
        .data
        .networks
        .iter()
        .filter(|n| matches(&n.name))
        .map(|n| {
            tree_item(format!("n:{}", n.name), &n.name)
                .detail(format!("{} · {}", n.driver, n.scope))
                .menu([
                    menu("docker.details", "open"),
                    menu("docker.remove", "ne.remove").enabled(!n.builtin()),
                ])
        })
        .collect();

    let registries: Vec<TreeItem> = app
        .registry_hosts()
        .into_iter()
        .filter(|host| matches(host) || matches(&crate::actions::registry_name(host)))
        .map(|host| {
            let logged = app.data.logged_in.contains(&host);
            tree_item(format!("r:{host}"), crate::actions::registry_name(&host))
                .icon(if logged { "●" } else { "○" })
                .detail(if logged {
                    t!("docker.logged_in").into_owned()
                } else {
                    String::new()
                })
                .menu([
                    menu("docker.browse", "open"),
                    menu("docker.login", "rg.login"),
                    menu("docker.logout", "rg.logout").enabled(logged),
                    menu("docker.forget", "rg.forget")
                        .enabled(!logged && app.registries.contains(&host)),
                ])
        })
        .collect();

    tree(
        "tree",
        [
            group(
                "g.compose",
                "docker.compose",
                app.data.projects().len().to_string(),
                projects,
            ),
            group(
                "g.containers",
                "docker.containers",
                format!("{alone_running}/{alone_total}"),
                containers,
            )
            .menu([
                menu("docker.new_container", "ct.new"),
                menu("docker.prune_containers", "ct.prune"),
            ]),
            group(
                "g.images",
                "docker.images",
                app.data.images.len().to_string(),
                images,
            )
            .menu([
                menu("docker.pull_image", "im.pull_new"),
                menu("docker.load_image", "im.load"),
                menu("docker.prune_images", "im.prune"),
            ]),
            group(
                "g.volumes",
                "docker.volumes",
                app.data.volumes.len().to_string(),
                volumes,
            )
            .menu([
                menu("docker.new_volume", "vo.new"),
                menu("docker.prune_volumes", "vo.prune"),
            ]),
            group(
                "g.networks",
                "docker.networks",
                app.data.networks.len().to_string(),
                networks,
            )
            .menu([
                menu("docker.new_network", "ne.new"),
                menu("docker.prune_networks", "ne.prune"),
            ]),
            group(
                "g.registries",
                "docker.registries",
                app.registry_hosts().len().to_string(),
                registries,
            )
            .menu([menu("docker.add_registry", "rg.add")]),
        ],
    )
}

/// The buttons of the Docker file in front.
fn file_block(app: &App) -> Option<Node> {
    let file = app.file.as_ref()?;
    let name = file.path.file_name()?.to_string_lossy().into_owned();
    let mut rows = vec![heading(name)];
    match file.kind {
        Kind::Compose => {
            // Two rows: the panel on the left is narrow.
            rows.push(
                row([
                    labelled("docker.up", "file.up", "play"),
                    labelled("docker.down", "file.down", "square"),
                ])
                .spacing(4.0),
            );
            rows.push(
                row([
                    picto("docker.logs", "file.logs", "file"),
                    picto("docker.build", "file.build", "zap"),
                    picto("docker.more", "file.more", "sliders-horizontal"),
                ])
                .spacing(2.0),
            );
            let services: Vec<Item> = file
                .targets
                .iter()
                .filter_map(|t| match &t.what {
                    What::Service(name) => Some(item(t.line.to_string(), name.clone()).icon("▶")),
                    _ => None,
                })
                .collect();
            if !services.is_empty() {
                rows.push(list("file.service", services));
            }
        }
        Kind::Dockerfile => {
            rows.push(
                row([
                    labelled("docker.build_run", "file.build_run", "play"),
                    labelled("docker.build", "file.build", "zap"),
                    picto("docker.more", "file.more", "sliders-horizontal"),
                ])
                .spacing(4.0),
            );
        }
    }
    Some(column(rows).spacing(4.0))
}

pub fn panel(app: &App) -> Node {
    let mut body = vec![row([
        picto("docker.refresh", "refresh", "refresh-cw"),
        picto("docker.new_container", "ct.new", "plus"),
        picto("docker.pull_image", "im.pull_new", "download"),
        picto("docker.cleanup", "cleanup", "sparkles"),
        picto("docker.console", "console", "terminal"),
    ])
    .spacing(2.0)];
    // A choice only when there is one to make.
    if app.data.contexts.len() > 1 {
        let current = app.cli.context.clone().or_else(|| {
            app.data
                .contexts
                .iter()
                .find(|c| c.current)
                .map(|c| c.name.clone())
        });
        body.push(select(
            "context",
            app.data.contexts.iter().map(|c| c.name.clone()),
            current,
        ));
    }
    if let Some(block) = file_block(app) {
        body.push(block);
        body.push(separator());
    }
    for busy in &app.busy {
        body.push(muted(format!("… {busy}")));
    }
    if let Some(error) = error_line(&app.error) {
        body.push(error);
        body.push(button(t!("docker.retry"), "refresh"));
    }
    if !app.loaded {
        if app.error.is_empty() {
            body.push(muted(t!("docker.loading")));
        }
    } else {
        body.push(input("filter", &app.filter, t!("docker.filter")));
        body.push(scroll(engine_tree(app)));
    }
    column(body).spacing(6.0).padding(6.0)
}

pub fn console(app: &App) -> Node {
    let mut bar = Vec::new();
    for (i, tab) in app.consoles.iter().enumerate() {
        let title = match tab.session.code() {
            None => tab.title.clone(),
            Some(0) => format!("✓ {}", tab.title),
            Some(code) => format!("✗ {} ({code})", tab.title),
        };
        bar.push(
            button(title, format!("tab:{i}")).button_style(if i == app.console {
                ButtonStyle::Outline
            } else {
                ButtonStyle::Ghost
            }),
        );
        bar.push(picto("docker.close", &format!("close:{i}"), "x"));
    }
    let mut body = vec![row(bar).spacing(4.0)];
    match app.consoles.get(app.console) {
        Some(tab) => {
            let screen = tab.session.with_screen(|s| {
                crate::snapshot::screen("screen", s, std::path::Path::new(""), "")
            });
            if let Some(screen) = screen {
                body.push(Node::Screen(Box::new(screen)));
            }
        }
        None => body.push(muted(t!("docker.console_empty"))),
    }
    column(body).spacing(4.0)
}
