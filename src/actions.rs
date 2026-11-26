//! What the user asks: from the rows of the tree, the buttons of the tabs,
//! the dialogs and the gutter of the Docker files. All of it on the main
//! thread, so that the editor can be asked questions.

use std::path::{Path, PathBuf};

use ardoise_plugin::protocol::{TreeAction, UiEvent};
use ardoise_plugin::ui::{GridEvent, GridSelection};
use rust_i18n::t;

use crate::commands::{self, words, Compose, Form};
use crate::ctx::Ctx;
use crate::dockerfile::{self, Kind, What};
use crate::engine::{name_arg, short};
use crate::files;
use crate::registry;
use crate::state::*;

/// What an action is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Object {
    Project(String),
    Service(String, String),
    Container(String),
    Image(String),
    Volume(String),
    Network(String),
    Registry(String),
}

impl Object {
    /// The object of a row of the tree.
    pub fn of_row(id: &str) -> Option<Object> {
        let (kind, rest) = id.split_once(':')?;
        let rest = rest.to_string();
        Some(match kind {
            "p" => Object::Project(rest),
            "s" => {
                let (project, service) = rest.split_once('/')?;
                Object::Service(project.into(), service.into())
            }
            "c" => Object::Container(rest),
            "i" => Object::Image(rest),
            "v" => Object::Volume(rest),
            "n" => Object::Network(rest),
            "r" => Object::Registry(rest),
            _ => return None,
        })
    }
}

fn confirm(ctx: &Ctx, message: impl Into<String>) -> bool {
    ctx.host.confirm(message).unwrap_or(false)
}

/// The object a tab shows.
fn viewed(ctx: &Ctx, view: &str) -> Option<Object> {
    Some(match ctx.lock().views.get(view)? {
        View::Container(v) => Object::Container(v.id.clone()),
        View::Image(v) => Object::Image(v.id.clone()),
        View::Object(v) if v.kind == ObjectKind::Volume => Object::Volume(v.name.clone()),
        View::Object(v) => Object::Network(v.name.clone()),
        View::Registry(v) => Object::Registry(v.host.clone()),
        View::Dialog(_) => return None,
    })
}

/// The id of the tab of a registry (a tab id has no `:`).
pub fn registry_tab(host: &str) -> String {
    format!("reg-{}", host.replace(':', "_").replace(['[', ']'], ""))
}

/// How a registry is named to the user.
pub fn registry_name(host: &str) -> String {
    if host == registry::HUB {
        "Docker Hub".into()
    } else {
        host.to_string()
    }
}

/// Opens the tab of an object, on one of its parts.
pub fn open(ctx: &Ctx, object: &Object, part: &str) {
    match object {
        Object::Container(key) => {
            let Some((id, name)) = ctx
                .lock()
                .data
                .container(key)
                .map(|c| (c.id.clone(), c.name.clone()))
            else {
                return;
            };
            let view = ContainerView {
                id: id.clone(),
                tab: part.to_string(),
                files: FilesView {
                    path: "/".into(),
                    ..FilesView::default()
                },
                ..ContainerView::default()
            };
            ctx.open_view(format!("ct-{}", short(&id)), name, View::Container(view));
        }
        Object::Image(reference) => {
            let Some(image) = ctx
                .lock()
                .data
                .images
                .iter()
                .find(|i| &i.reference() == reference || short(&i.id) == short(reference))
                .cloned()
            else {
                return;
            };
            let view = ImageView {
                id: image.id.clone(),
                tab: part.to_string(),
                ..ImageView::default()
            };
            ctx.open_view(
                format!("im-{}", short(&image.id)),
                image.reference(),
                View::Image(view),
            );
        }
        Object::Volume(name) | Object::Network(name) => {
            let (kind, id) = if matches!(object, Object::Volume(_)) {
                (ObjectKind::Volume, format!("vol-{name}"))
            } else {
                (ObjectKind::Network, format!("net-{name}"))
            };
            let view = ObjectView {
                kind,
                name: name.clone(),
                // A volume opens on what it holds.
                tab: if part == "inspect" {
                    "inspect"
                } else {
                    "files"
                }
                .into(),
                files: FilesView {
                    path: "/".into(),
                    ..FilesView::default()
                },
                inspect: None,
                picked: None,
                error: String::new(),
            };
            ctx.open_view(id, name.clone(), View::Object(view));
        }
        Object::Registry(host) => {
            let view = RegistryView {
                host: host.clone(),
                ..RegistryView::default()
            };
            ctx.open_view(
                registry_tab(host),
                registry_name(host),
                View::Registry(view),
            );
        }
        Object::Project(_) | Object::Service(..) => {}
    }
}

/// A tab the editor brings back after a restart, from its id.
pub fn restore(ctx: &Ctx, id: &str) {
    if !ctx.lock().loaded {
        ctx.reload();
    }
    let object = if let Some(key) = id.strip_prefix("ct-") {
        Object::Container(key.into())
    } else if let Some(key) = id.strip_prefix("im-") {
        Object::Image(key.into())
    } else if let Some(name) = id.strip_prefix("vol-") {
        Object::Volume(name.into())
    } else if let Some(name) = id.strip_prefix("net-") {
        Object::Network(name.into())
    } else if id.starts_with("reg-") {
        let hosts = ctx.lock().registry_hosts();
        match hosts.into_iter().find(|h| registry_tab(h) == id) {
            Some(host) => Object::Registry(host),
            None => return,
        }
    } else {
        return;
    };
    let part = "info";
    open(ctx, &object, part);
}

pub fn open_dialog(ctx: &Ctx, kind: DialogKind, setup: impl FnOnce(&mut Form)) {
    let (id, title) = match &kind {
        DialogKind::Run => ("dlg-run", t!("docker.new_container")),
        DialogKind::Pull => ("dlg-pull", t!("docker.pull_image")),
        DialogKind::Build => ("dlg-build", t!("docker.build")),
        DialogKind::Volume => ("dlg-volume", t!("docker.new_volume")),
        DialogKind::Network => ("dlg-network", t!("docker.new_network")),
        DialogKind::Tag(_) => ("dlg-tag", t!("docker.tag")),
        DialogKind::Exec(_) => ("dlg-exec", t!("docker.exec")),
        DialogKind::Commit(_) => ("dlg-commit", t!("docker.commit")),
        DialogKind::Scale(_) => ("dlg-scale", t!("docker.scale")),
        DialogKind::File(..) => ("dlg-file", t!("docker.run_file")),
        DialogKind::Cleanup => ("dlg-clean", t!("docker.cleanup")),
        DialogKind::Login => ("dlg-login", t!("docker.registry_login")),
        DialogKind::Push(_) => ("dlg-push", t!("docker.push_to")),
    };
    let mut title = title.into_owned();
    if let DialogKind::File(path, line) = &kind {
        let app = ctx.lock();
        let what = app
            .file
            .as_ref()
            .and_then(|f| f.targets.iter().find(|t| Some(t.line) == *line))
            .and_then(|t| match &t.what {
                What::Service(name) => Some(name.clone()),
                What::Stage(name) => name.clone(),
                What::All => None,
            })
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()));
        if let Some(what) = what {
            title = what;
        }
    }
    let measure = kind == DialogKind::Cleanup;
    let mut dialog = Dialog::new(kind);
    setup(&mut dialog.form);
    ctx.open_view(id.to_string(), title, View::Dialog(Box::new(dialog)));
    if measure {
        measure_cleanup(ctx);
    }
}

pub fn open_cleanup(ctx: &Ctx) {
    open_dialog(ctx, DialogKind::Cleanup, |form| {
        // Only what costs nothing to lose is ticked at first.
        form.flag("dangling", true);
        form.flag("cache", true);
    });
}

pub fn open_run(ctx: &Ctx, image: &str) {
    open_dialog(ctx, DialogKind::Run, |form| {
        form.set("image", image);
        form.flag("detach", true);
    });
}

/// How much room is taken, and what each box would remove.
pub fn measure_cleanup(ctx: &Ctx) {
    const ID: &str = "dlg-clean";
    let (ctx, cli) = (ctx.clone(), ctx.cli());
    std::thread::spawn(move || {
        let preview = (|| -> Result<Preview, String> {
            let mut preview = Preview {
                usage: cli.disk_usage()?,
                ..Preview::default()
            };
            let containers = cli.containers()?;
            preview.items.insert(
                "stopped",
                containers
                    .iter()
                    .filter(|c| c.prunable())
                    .map(|c| c.name.clone())
                    .collect(),
            );
            let images = cli.images()?;
            let used: Vec<String> = cli
                .used_image_ids()?
                .iter()
                .map(|id| short(id).to_string())
                .collect();
            let unused = |dangling_only: bool| {
                images
                    .iter()
                    .filter(|i| !used.iter().any(|u| u == short(&i.id)))
                    .filter(|i| !dangling_only || i.dangling())
                    .map(|i| {
                        if i.dangling() {
                            short(&i.id).to_string()
                        } else {
                            i.reference()
                        }
                    })
                    .collect::<Vec<_>>()
            };
            preview.items.insert("dangling", unused(true));
            preview.items.insert("all_images", unused(false));
            let networks = cli.run(&[
                "network",
                "ls",
                "--filter",
                "dangling=true",
                "--format",
                "{{.Name}}",
            ])?;
            preview.items.insert(
                "networks",
                networks
                    .lines()
                    .filter(|n| !matches!(*n, "bridge" | "host" | "none"))
                    .map(str::to_string)
                    .collect(),
            );
            let volumes = cli.unused_volumes()?;
            preview.items.insert(
                "anonymous_volumes",
                volumes
                    .iter()
                    .filter(|v| anonymous(v))
                    .map(|v| v[..12].to_string())
                    .collect(),
            );
            preview.items.insert("all_volumes", volumes);
            Ok(preview)
        })();
        if let Some(View::Dialog(dialog)) = ctx.lock().views.get_mut(ID) {
            match preview {
                Ok(preview) => dialog.preview = Some(preview),
                Err(e) => dialog.error = e,
            }
        }
        ctx.draw_view(ID);
    });
}

/// A volume `docker` named itself.
fn anonymous(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

fn run_cleanup(ctx: &Ctx, id: &str, form: &Form) -> Result<(), String> {
    let commands = commands::cleanup(form)?;
    if commands.is_empty() {
        return Err(t!("docker.nothing_ticked").to_string());
    }
    let lines: Vec<String> = commands
        .iter()
        .map(|c| format!("docker {}", c.join(" ")))
        .collect();
    let question = format!("{}\n\n{}", t!("docker.confirm_cleanup"), lines.join("\n"));
    if !confirm(ctx, question) {
        return Ok(());
    }
    if let Some(View::Dialog(dialog)) = ctx.lock().views.get_mut(id) {
        dialog.busy = true;
        dialog.report.clear();
    }
    ctx.draw_view(id);
    let (ctx, cli, id) = (ctx.clone(), ctx.cli(), id.to_string());
    std::thread::spawn(move || {
        let mut report = Vec::new();
        for (command, line) in commands.iter().zip(lines) {
            match cli.run_long(command) {
                Ok(out) => {
                    let freed = out
                        .lines()
                        .rev()
                        .find(|l| l.starts_with("Total"))
                        .unwrap_or("✓");
                    report.push(format!("{line} → {freed}"));
                }
                Err(e) => report.push(format!("{line} ✗ {e}")),
            }
        }
        if let Some(View::Dialog(dialog)) = ctx.lock().views.get_mut(&id) {
            dialog.busy = false;
            dialog.report = report.join("\n");
        }
        ctx.draw_view(&id);
        measure_cleanup(&ctx);
        ctx.reload();
    });
    Ok(())
}

// ── Containers, images, volumes, networks ──────────────────────────────

fn container_action(ctx: &Ctx, verb: &str, key: &str) {
    let Some(container) = ctx.lock().data.container(key).cloned() else {
        return;
    };
    let (id, name) = (container.id.as_str(), container.name.clone());
    let simple = |label: &str| ctx.job(format!("{} {name}", t!(label)), vec![words(&[verb, id])]);
    let title = |label: &str| format!("{} {name}", t!(label));
    match verb {
        "start" => simple("docker.start"),
        "stop" => simple("docker.stop"),
        "restart" => simple("docker.restart"),
        "pause" => simple("docker.pause"),
        "unpause" => simple("docker.unpause"),
        "kill" => {
            if confirm(ctx, t!("docker.confirm_kill", name = name)) {
                simple("docker.kill");
            }
        }
        "remove" => {
            let alive = container.running() || container.paused();
            let question = if alive {
                t!("docker.confirm_remove_running", name = name)
            } else {
                t!("docker.confirm_remove", name = name)
            };
            if confirm(ctx, question) {
                let mut args = words(&["rm"]);
                if alive {
                    args.push("-f".into());
                }
                args.push(id.into());
                ctx.job(title("docker.remove"), vec![args]);
            }
        }
        "logs" => ctx.console(
            title("docker.logs"),
            words(&["logs", "-f", "--tail", "1000", id]),
            None,
        ),
        "shell" => ctx.console(name.clone(), commands::shell(id, false), None),
        "shell_root" => ctx.console(format!("root@{name}"), commands::shell(id, true), None),
        // Ctrl+C leaves the container running.
        "attach" => ctx.console(
            title("docker.attach"),
            words(&["attach", "--sig-proxy=false", id]),
            None,
        ),
        "exec" => open_dialog(ctx, DialogKind::Exec(id.into()), |_| {}),
        "commit" => open_dialog(ctx, DialogKind::Commit(id.into()), |_| {}),
        "files" => open(ctx, &Object::Container(id.into()), "files"),
        "copy_id" => ctx.host.copy(id),
        _ => {}
    }
}

fn image_action(ctx: &Ctx, verb: &str, key: &str) {
    let Some(image) = ctx
        .lock()
        .data
        .images
        .iter()
        .find(|i| i.reference() == key || i.id == key)
        .cloned()
    else {
        return;
    };
    let reference = image.reference();
    let title = |label: &str| format!("{} {reference}", t!(label));
    match verb {
        "run" => open_run(ctx, &reference),
        "layers" => open(ctx, &Object::Image(reference.clone()), "layers"),
        "pull" => ctx.console(title("docker.pull"), words(&["pull", &reference]), None),
        "push" => open_push(ctx, &reference),
        "tag" => open_dialog(ctx, DialogKind::Tag(reference.clone()), |_| {}),
        "save" => {
            let file = format!("{}.tar", reference.replace(['/', ':'], "_"));
            if let Ok(Some(path)) = ctx.host.pick_save_path(file) {
                let path = path.to_string_lossy().into_owned();
                ctx.job(
                    title("docker.save_image"),
                    vec![words(&["save", "-o", &path, &reference])],
                );
            }
        }
        "copy_id" => ctx.host.copy(image.id.clone()),
        "remove" | "force_remove" => {
            if !confirm(ctx, t!("docker.confirm_remove", name = reference)) {
                return;
            }
            let mut args = words(&["rmi"]);
            if verb == "force_remove" {
                args.push("-f".into());
            }
            args.push(reference.clone());
            ctx.job(title("docker.remove"), vec![args]);
        }
        _ => {}
    }
}

fn prune(ctx: &Ctx, question: &str, args: &[&str]) {
    if confirm(ctx, t!(question)) {
        ctx.job(t!("docker.cleanup").to_string(), vec![words(args)]);
    }
}

/// Where an image goes: the registry its name says, else Docker Hub.
pub fn push_target(image: &str) -> (String, String, String) {
    let (name, tag) = match image.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name, tag),
        _ => (image, "latest"),
    };
    // The first part is a host if it looks like one.
    match name.split_once('/') {
        Some((first, rest)) if first.contains(['.', ':']) || first == "localhost" => {
            (registry::host(first), rest.into(), tag.into())
        }
        _ => (registry::HUB.into(), name.into(), tag.into()),
    }
}

fn open_push(ctx: &Ctx, image: &str) {
    let (host, repository, tag) = push_target(image);
    open_dialog(ctx, DialogKind::Push(image.to_string()), |form| {
        form.set("registry", host);
        // An image without a name has none to offer.
        if !image.starts_with("sha256:") && image.contains(':') {
            form.set("repository", repository);
            form.set("tag", tag);
        }
    });
}

fn save_registries(app: &App) {
    if let Ok(json) = serde_json::to_string(&app.registries) {
        let _ = std::fs::write(app.data_dir.join("registries.json"), json);
    }
}

/// `docker login`, the password on its standard input only.
fn log_in(ctx: &Ctx, id: &str, form: &Form) -> Result<(), String> {
    let host = registry::host(form.get("server"));
    if !registry::good_host(&host) {
        return Err(t!("docker.bad_name", name = host).to_string());
    }
    let user = name_arg(form.get("user"))?.to_string();
    let password = form.get_raw("password").to_string();
    if password.is_empty() {
        return Err(t!("docker.no_password").to_string());
    }
    let mut args = words(&["login", "-u", &user, "--password-stdin"]);
    if host != registry::HUB {
        args.push(host.clone());
    }
    if let Some(View::Dialog(dialog)) = ctx.lock().views.get_mut(id) {
        dialog.busy = true;
        dialog.error.clear();
    }
    ctx.draw_view(id);
    let (ctx, cli, id) = (ctx.clone(), ctx.cli(), id.to_string());
    std::thread::spawn(move || {
        let limit = std::time::Duration::from_secs(60);
        match cli.run_with(&args, Some(password.into_bytes()), limit) {
            Ok(_) => {
                ctx.close_view(&id);
                {
                    let mut app = ctx.lock();
                    app.registries.insert(host.clone());
                    save_registries(&app);
                }
                ctx.host
                    .notify(t!("docker.logged_in_to", name = registry_name(&host)));
                ctx.reload();
            }
            Err(e) => {
                if let Some(View::Dialog(dialog)) = ctx.lock().views.get_mut(&id) {
                    dialog.busy = false;
                    dialog.error = e;
                }
                ctx.draw_view(&id);
            }
        }
    });
    Ok(())
}

fn registry_action(ctx: &Ctx, verb: &str, host: &str) {
    match verb {
        "login" => {
            let server = host.to_string();
            open_dialog(ctx, DialogKind::Login, |form| form.set("server", server));
        }
        "logout" => {
            let mut args = words(&["logout"]);
            if host != registry::HUB {
                args.push(host.to_string());
            }
            if confirm(ctx, t!("docker.confirm_logout", name = registry_name(host))) {
                ctx.job(
                    format!("{} {}", t!("docker.logout"), registry_name(host)),
                    vec![args],
                );
            }
        }
        "forget" => {
            {
                let mut app = ctx.lock();
                app.registries.remove(host);
                save_registries(&app);
            }
            ctx.draw_all();
        }
        _ => {}
    }
}

/// An event of the tab of a registry.
fn registry_event(ctx: &Ctx, id: &str, event: &UiEvent) {
    let Some((host, typed, search, repositories, repository, tags, picked_tag)) =
        (match ctx.lock().views.get(id) {
            Some(View::Registry(v)) => Some((
                v.host.clone(),
                v.typed.clone(),
                v.search.clone(),
                v.repositories.clone(),
                v.repository.clone(),
                v.tags.clone(),
                v.tag,
            )),
            _ => None,
        })
    else {
        return;
    };
    let change = |change: &dyn Fn(&mut RegistryView)| {
        if let Some(View::Registry(view)) = ctx.lock().views.get_mut(id) {
            change(view);
        }
    };
    // The image of a row of the tags (or of the row picked).
    let image = |row: Option<usize>| {
        let tag = tags.get(row?)?;
        Some(registry::reference(&host, repository.as_deref()?, tag))
    };
    let action = event.action.as_str();
    match action {
        "reg.typed" | "reg.search" => {
            let value = event.text().unwrap_or_default().to_string();
            change(&|v| {
                if action == "reg.typed" {
                    v.typed = value.clone();
                } else {
                    v.search = value.clone();
                }
            });
            ctx.draw_view(id);
        }
        "reg.tags" => ctx.load_tags(id, typed.trim().to_string()),
        "reg.find" => ctx.search_hub(id, search),
        "repositories" => match GridEvent::from_event(event) {
            Some(GridEvent::Select { cursor, .. }) => {
                if let Some((name, _)) = repositories.get(cursor.0) {
                    change(&|v| v.typed = name.clone());
                    ctx.load_tags(id, name.clone());
                }
            }
            Some(GridEvent::Open { row, .. }) => {
                if let Some((name, _)) = repositories.get(row) {
                    ctx.load_tags(id, name.clone());
                }
            }
            _ => {}
        },
        "tags" => match GridEvent::from_event(event) {
            Some(GridEvent::Select { cursor, .. }) => {
                change(&|v| v.tag = Some(cursor.0));
                ctx.draw_view(id);
            }
            Some(GridEvent::Open { row, .. }) => {
                if let Some(image) = image(Some(row)) {
                    open_run(ctx, &image);
                }
            }
            _ => {}
        },
        "reg.pull" => {
            if let Some(image) = image(picked_tag) {
                let args = words(&["pull", &image]);
                ctx.console(format!("{} {image}", t!("docker.pull")), args, None);
            }
        }
        "reg.run" => {
            if let Some(image) = image(picked_tag) {
                open_run(ctx, &image);
            }
        }
        "reg.copy" => {
            if let Some(image) = image(picked_tag) {
                ctx.host.copy(image);
            }
        }
        action => {
            if let Some(verb) = action.strip_prefix("rg.") {
                registry_action(ctx, verb, &host);
            }
        }
    }
}

/// An action that needs no object: the rows of the groups, the toolbar.
fn general_action(ctx: &Ctx, action: &str) -> bool {
    match action {
        "ct.new" => open_run(ctx, ""),
        "ct.prune" => prune(
            ctx,
            "docker.confirm_prune_containers",
            &["container", "prune", "-f"],
        ),
        "im.pull_new" => open_dialog(ctx, DialogKind::Pull, |_| {}),
        "im.load" => {
            if let Ok(Some(path)) = ctx
                .host
                .pick_file(t!("docker.load_image"), &["tar", "gz", "tgz"])
            {
                let path = path.to_string_lossy().into_owned();
                ctx.job(
                    t!("docker.load_image").to_string(),
                    vec![words(&["load", "-i", &path])],
                );
            }
        }
        "im.prune" => prune(
            ctx,
            "docker.confirm_prune_images",
            &["image", "prune", "-f"],
        ),
        "vo.new" => open_dialog(ctx, DialogKind::Volume, |_| {}),
        "vo.prune" => prune(
            ctx,
            "docker.confirm_prune_volumes",
            &["volume", "prune", "-f"],
        ),
        "ne.new" => open_dialog(ctx, DialogKind::Network, |_| {}),
        "ne.prune" => prune(
            ctx,
            "docker.confirm_prune_networks",
            &["network", "prune", "-f"],
        ),
        "cleanup" => open_cleanup(ctx),
        "rg.add" => open_dialog(ctx, DialogKind::Login, |_| {}),
        _ => return false,
    }
    true
}

// ── Compose ────────────────────────────────────────────────────────────

/// A project as its containers say it was started.
pub fn compose_of(data: &Data, project: &str, service: Option<&str>) -> Compose {
    let any = data.containers.iter().find(|c| c.project == project);
    Compose {
        project: Some(project.to_string()),
        files: any
            .map(|c| {
                c.files
                    .split(',')
                    .filter(|f| !f.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default(),
        dir: any
            .filter(|c| !c.dir.is_empty())
            .map(|c| PathBuf::from(&c.dir)),
        service: service.map(str::to_string),
    }
}

/// A compose file of the editor. Given by `-f`, Compose no longer reads the
/// override beside it by itself: it is given too.
pub fn compose_file(path: &Path, service: Option<String>) -> Compose {
    let mut list = vec![path.to_path_buf()];
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if let Some((stem @ ("compose" | "docker-compose"), _)) = name.rsplit_once('.') {
        let beside = ["yml", "yaml"]
            .iter()
            .map(|ext| path.with_file_name(format!("{stem}.override.{ext}")))
            .find(|f| f.is_file());
        list.extend(beside);
    }
    Compose {
        files: list,
        service,
        ..Compose::default()
    }
}

fn compose_action(ctx: &Ctx, verb: &str, compose: Compose) {
    let name = compose
        .service
        .clone()
        .or_else(|| compose.project.clone())
        .or_else(|| {
            compose
                .files
                .first()
                .and_then(|f| f.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let dir = compose.dir.clone().or_else(|| {
        compose
            .files
            .first()
            .and_then(|f| f.parent())
            .map(Path::to_path_buf)
    });
    match verb {
        "open_file" => {
            match compose.files.iter().find(|f| f.is_file()) {
                Some(file) => {
                    if let Err(e) = ctx.host.open_file(file, None) {
                        ctx.host.error(e.message);
                    }
                }
                None => ctx.host.error(t!("docker.no_file")),
            }
            return;
        }
        "scale" => {
            open_dialog(ctx, DialogKind::Scale(compose), |_| {});
            return;
        }
        "shell" => {
            if let Some(service) = &compose.service {
                let mut args = compose.base();
                args.extend(words(&[
                    "exec",
                    service,
                    "sh",
                    "-c",
                    "command -v bash >/dev/null 2>&1 && exec bash || exec sh",
                ]));
                ctx.console(name, args, dir.as_deref());
            }
            return;
        }
        "downv" | "downall" if !confirm(ctx, t!("docker.confirm_down_volumes", name = name)) => {
            return;
        }
        _ => {}
    }
    if let Some(args) = compose.args(verb) {
        ctx.console(format!("compose {verb} {name}"), args, dir.as_deref());
    }
}

// ── The Docker files ───────────────────────────────────────────────────

/// The file in front: its marks in the gutter, its buttons in the panel.
pub fn set_file(ctx: &Ctx, path: Option<&Path>) {
    let file = path.and_then(|path| {
        let kind = dockerfile::kind(path)?;
        let text = std::fs::read_to_string(path).ok()?;
        Some(DockerFile {
            path: path.to_path_buf(),
            kind,
            targets: dockerfile::targets(kind, &text),
        })
    });
    if let Some(file) = &file {
        ctx.host
            .set_run_marks(&file.path, file.targets.iter().map(|t| t.line).collect());
        if file.kind == Kind::Dockerfile {
            ctx.host
                .set_completer(&file.path, Some(crate::complete::TRIGGERS));
        }
    }
    // The images offered after `FROM` are those of the engine.
    let wanted = file.as_ref().is_some_and(|f| f.kind == Kind::Dockerfile) && !ctx.lock().loaded;
    ctx.lock().file = file;
    ctx.draw_panel();
    if wanted {
        ctx.refresh();
    }
}

/// The `docker build` of a Dockerfile as it is run straight away.
fn quick_build(path: &Path, target: Option<&str>) -> Form {
    let mut form = Form::default();
    form.set("file", path.to_string_lossy());
    let context = path.parent().unwrap_or(Path::new("."));
    form.set("context", context.to_string_lossy());
    form.set("tag", dockerfile::default_tag(path));
    if let Some(target) = target {
        form.set("target", target);
    }
    form
}

/// Builds, then runs the image with its ports published.
fn build_and_run(ctx: &Ctx, form: &Form) -> Result<(), String> {
    let mut form = form.clone();
    let tags = crate::engine::split_words(form.get("tag"))?;
    let tag = match tags.first() {
        Some(tag) => name_arg(tag)?.to_string(),
        None => {
            let tag = dockerfile::default_tag(Path::new(form.get("file")));
            form.set("tag", tag.clone());
            tag
        }
    };
    let dir = PathBuf::from(form.get("context"));
    ctx.console_chain(
        format!("{} {tag}", t!("docker.build_run")),
        vec![
            commands::build(&form)?,
            words(&["run", "--rm", "-it", "-P", &tag]),
        ],
        Some(&dir),
    );
    Ok(())
}

/// A button on the file in front, for the target at `line` (none: the whole
/// file).
fn file_action(ctx: &Ctx, verb: &str, line: Option<usize>) {
    let Some(file) = ctx.lock().file.clone() else {
        return;
    };
    if verb == "more" {
        open_dialog(ctx, DialogKind::File(file.path, None), |_| {});
        return;
    }
    let target = line.and_then(|l| file.targets.iter().find(|t| t.line == l));
    match file.kind {
        Kind::Compose => {
            let service = target.and_then(|t| match &t.what {
                What::Service(name) => Some(name.clone()),
                _ => None,
            });
            compose_action(ctx, verb, compose_file(&file.path, service));
        }
        Kind::Dockerfile => {
            // The last stage is what a build makes by itself.
            let last = file.targets.last().map(|t| t.line);
            let stage = target.filter(|t| Some(t.line) != last);
            let name = stage.and_then(|t| match &t.what {
                What::Stage(name) => name.clone(),
                _ => None,
            });
            let form = quick_build(&file.path, name.as_deref());
            let dir = PathBuf::from(form.get("context"));
            let result = match verb {
                "build" => commands::build(&form).map(|args| {
                    ctx.console(
                        format!("{} {}", t!("docker.build"), form.get("tag")),
                        args,
                        Some(&dir),
                    )
                }),
                "build_run" => build_and_run(ctx, &form),
                "build_options" => {
                    open_dialog(ctx, DialogKind::Build, |f| *f = form.clone());
                    Ok(())
                }
                "check" => {
                    let path = file.path.to_string_lossy().into_owned();
                    ctx.console(
                        t!("docker.check").to_string(),
                        words(&["build", "--check", "-f", &path, form.get("context")]),
                        Some(&dir),
                    );
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(e) = result {
                ctx.host.error(e);
            }
        }
    }
}

// ── Dialogs ────────────────────────────────────────────────────────────

/// What the main button of a dialog does. `Ok(true)`: the dialog closes.
fn submit(
    ctx: &Ctx,
    id: &str,
    action: &str,
    kind: &DialogKind,
    form: &Form,
) -> Result<bool, String> {
    match kind {
        DialogKind::Run => {
            let create = action == "dlg.create";
            let args = commands::run_container(form, create)?;
            let title = format!("{} {}", t!("docker.run"), form.get("image"));
            ctx.console(title, args, None);
        }
        DialogKind::Pull => {
            let image = name_arg(form.get("image"))?;
            ctx.console(
                format!("{} {image}", t!("docker.pull")),
                words(&["pull", image]),
                None,
            );
        }
        DialogKind::Build => {
            if action == "dlg.build_run" {
                build_and_run(ctx, form)?;
            } else {
                let dir = PathBuf::from(form.get("context"));
                ctx.console(
                    format!("{} {}", t!("docker.build"), form.get("tag")),
                    commands::build(form)?,
                    Some(&dir),
                );
            }
        }
        DialogKind::Volume => {
            let name = name_arg(form.get("name"))?;
            let mut args = words(&["volume", "create"]);
            if !form.get("driver").is_empty() {
                args.extend(words(&["--driver", name_arg(form.get("driver"))?]));
            }
            args.push(name.into());
            ctx.job(format!("{} {name}", t!("docker.new_volume")), vec![args]);
        }
        DialogKind::Network => {
            let name = name_arg(form.get("name"))?;
            let mut args = words(&["network", "create"]);
            for (field, flag) in [("driver", "--driver"), ("subnet", "--subnet")] {
                if !form.get(field).is_empty() {
                    args.extend(words(&[flag, name_arg(form.get(field))?]));
                }
            }
            if form.on("internal") {
                args.push("--internal".into());
            }
            args.push(name.into());
            ctx.job(format!("{} {name}", t!("docker.new_network")), vec![args]);
        }
        DialogKind::Tag(image) => {
            let tag = name_arg(form.get("tag"))?;
            ctx.job(
                format!("{} {tag}", t!("docker.tag")),
                vec![words(&["tag", image, tag])],
            );
        }
        DialogKind::Exec(container) => {
            let command = form.get("command");
            if command.is_empty() {
                return Err(t!("docker.no_command").to_string());
            }
            let mut args = words(&["exec", "-it"]);
            for (field, flag) in [("user", "-u"), ("workdir", "-w")] {
                if !form.get(field).is_empty() {
                    args.extend(words(&[flag, name_arg(form.get(field))?]));
                }
            }
            args.extend(words(&[container, "sh", "-c", command]));
            ctx.console(command.to_string(), args, None);
        }
        DialogKind::Commit(container) => {
            let tag = name_arg(form.get("tag"))?;
            let mut args = words(&["commit"]);
            if !form.get("message").is_empty() {
                args.extend(words(&["-m", form.get("message")]));
            }
            args.extend(words(&[container, tag]));
            ctx.job(format!("{} {tag}", t!("docker.commit")), vec![args]);
        }
        DialogKind::Scale(compose) => {
            let count: u32 = form
                .get("count")
                .parse()
                .map_err(|_| t!("docker.bad_count").to_string())?;
            let service = compose.service.clone().unwrap_or_default();
            let mut args = compose.base();
            args.extend(words(&[
                "up",
                "-d",
                "--scale",
                &format!("{service}={count}"),
                &service,
            ]));
            ctx.console(
                format!("scale {service}={count}"),
                args,
                compose.dir.as_deref(),
            );
        }
        DialogKind::Cleanup => {
            run_cleanup(ctx, id, form)?;
            return Ok(false);
        }
        DialogKind::Login => {
            // Only looked into: no account.
            if action == "dlg.add" {
                let host = registry::host(form.get("server"));
                if !registry::good_host(&host) {
                    return Err(t!("docker.bad_name", name = host).to_string());
                }
                {
                    let mut app = ctx.lock();
                    app.registries.insert(host);
                    save_registries(&app);
                }
                ctx.draw_panel();
                return Ok(true);
            }
            log_in(ctx, id, form)?;
            return Ok(false);
        }
        DialogKind::Push(image) => {
            let host = registry::host(form.get("registry"));
            let repository = form.get("repository");
            if !registry::good_host(&host) || !registry::good_repository(repository) {
                return Err(t!("docker.bad_name", name = repository).to_string());
            }
            let tag = match form.get("tag") {
                "" => "latest",
                tag => name_arg(tag)?,
            };
            let target = registry::reference(&host, repository, tag);
            let mut commands = Vec::new();
            if &target != image {
                commands.push(words(&["tag", image, &target]));
            }
            commands.push(words(&["push", &target]));
            ctx.console_chain(format!("{} {target}", t!("docker.push")), commands, None);
        }
        DialogKind::File(..) => return Ok(false),
    }
    Ok(true)
}

fn dialog_event(ctx: &Ctx, id: &str, event: &UiEvent) {
    let action = event.action.as_str();
    if let Some(field) = action.strip_prefix("f.") {
        if let (Some(value), Some(View::Dialog(dialog))) =
            (event.text(), ctx.lock().views.get_mut(id))
        {
            dialog.form.set(field, value);
        }
        ctx.draw_view(id);
        return;
    }
    if let Some(flag) = action.strip_prefix("b.") {
        if let Some(View::Dialog(dialog)) = ctx.lock().views.get_mut(id) {
            dialog.form.flag(flag, event.checked().unwrap_or(false));
        }
        ctx.draw_view(id);
        return;
    }
    let Some((kind, form, busy)) = (match ctx.lock().views.get(id) {
        Some(View::Dialog(d)) => Some((d.kind.clone(), d.form.clone(), d.busy)),
        _ => None,
    }) else {
        return;
    };
    match action {
        "dlg.cancel" => ctx.close_view(id),
        "dlg.measure" => measure_cleanup(ctx),
        "dlg.ok" | "dlg.create" | "dlg.build_run" | "dlg.add" if !busy => {
            match submit(ctx, id, action, &kind, &form) {
                Ok(true) => ctx.close_view(id),
                Ok(false) => {}
                Err(e) => {
                    if let Some(View::Dialog(dialog)) = ctx.lock().views.get_mut(id) {
                        dialog.error = e;
                    }
                    ctx.draw_view(id);
                }
            }
        }
        action => {
            if let (Some(verb), DialogKind::File(_, line)) = (action.strip_prefix("file."), &kind) {
                ctx.close_view(id);
                file_action(ctx, verb, *line);
            }
        }
    }
}

// ── The files of a container ───────────────────────────────────────────

/// The rows picked in the listing.
pub fn picked(selection: Option<GridSelection>, count: usize) -> std::ops::Range<usize> {
    match selection {
        Some(s) => {
            let (a, b) = (s.anchor.0, s.cursor.0);
            a.min(b).min(count)..(a.max(b) + 1).min(count)
        }
        None => 0..0,
    }
}

fn go_to(ctx: &Ctx, id: &str, path: String) {
    ctx.with_files(id, |f| {
        f.path = path;
        f.typed = None;
        f.selection = None;
        f.asking = None;
        f.entries.clear();
    });
    ctx.list_files(id);
}

fn ask(ctx: &Ctx, id: &str, asking: Asking, value: String) {
    ctx.with_files(id, |f| f.asking = Some((asking, value)));
    ctx.draw_view(id);
}

/// What was typed for a new file, a rename or an upload.
fn answer(ctx: &Ctx, id: &str) {
    let Some((place, shown, root)) = ctx.files_of(id) else {
        return;
    };
    let dir = place.real(&shown);
    let mut asked = None;
    ctx.with_files(id, |f| asked = f.asking.clone());
    let Some((asking, value)) = asked else { return };
    let checked = match &asking {
        Asking::Upload => Ok(value.trim()),
        _ => files::file_name(&value),
    };
    let value = match checked {
        Ok(value) if !value.is_empty() => value.to_string(),
        other => {
            let error = other
                .err()
                .unwrap_or_else(|| t!("docker.bad_name", name = "").to_string());
            ctx.with_files(id, |f| f.error = error);
            ctx.draw_view(id);
            return;
        }
    };
    ctx.with_files(id, |f| f.asking = None);
    let path = files::join(&dir, &value);
    match asking {
        Asking::File => ctx.file_job(id, value.clone(), move |cli| {
            files::make_file(cli, &place.container(cli)?, root, &path)
        }),
        Asking::Folder => ctx.file_job(id, value.clone(), move |cli| {
            files::make_folder(cli, &place.container(cli)?, root, &path)
        }),
        Asking::Rename(old) => {
            let from = files::join(&dir, &old);
            ctx.file_job(id, format!("{old} → {value}"), move |cli| {
                files::rename(cli, &place.container(cli)?, root, &from, &path)
            });
        }
        Asking::Upload => {
            let local = PathBuf::from(value);
            ctx.file_job(id, local.display().to_string(), move |cli| {
                files::copy_in(cli, &place.container(cli)?, &local, &format!("{dir}/"))
            });
        }
    }
}

fn files_event(ctx: &Ctx, id: &str, event: &UiEvent) {
    let Some((place, shown, root)) = ctx.files_of(id) else {
        return;
    };
    let dir = place.real(&shown);
    let mut entries = Vec::new();
    let mut selection = None;
    ctx.with_files(id, |f| {
        entries = f.entries.clone();
        selection = f.selection;
    });
    let range = picked(selection, entries.len());
    let first = entries
        .get(range.start)
        .filter(|_| !range.is_empty())
        .cloned();
    let name = match &place {
        Place::Container(container) => ctx
            .lock()
            .data
            .container(container)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| short(container).to_string()),
        Place::Volume(volume) => volume.clone(),
    };

    // The listing itself: picking, opening, and its menu.
    let action = match event.action.as_str() {
        "files" => match GridEvent::from_event(event) {
            Some(GridEvent::Select { anchor, cursor }) => {
                ctx.with_files(id, |f| f.selection = Some(GridSelection { anchor, cursor }));
                ctx.draw_view(id);
                return;
            }
            Some(GridEvent::Open { row, .. }) => {
                ctx.with_files(id, |f| {
                    f.selection = Some(GridSelection {
                        anchor: (row, 0),
                        cursor: (row, 0),
                    })
                });
                let Some(entry) = entries.get(row) else {
                    return;
                };
                let path = files::join(&dir, &entry.name);
                if entry.folder {
                    go_to(ctx, id, files::join(&shown, &entry.name));
                } else {
                    ctx.edit_file(place, name, path, root);
                }
                return;
            }
            Some(GridEvent::Delete) => "fs.delete".to_string(),
            _ => return,
        },
        action => action.to_string(),
    };
    match action.as_str() {
        "fs.up" => go_to(ctx, id, files::parent(&shown)),
        "fs.refresh" => ctx.list_files(id),
        "fs.path" => {
            let typed = event.text().map(str::to_string);
            ctx.with_files(id, |f| f.typed = typed);
            ctx.draw_view(id);
        }
        "fs.go" => {
            if let Some(path) = event.text() {
                go_to(ctx, id, files::normalize(path));
            }
        }
        "fs.root" => {
            let on = event.checked().unwrap_or(false);
            ctx.with_files(id, |f| f.root = on);
            ctx.list_files(id);
        }
        "fs.new_file" => ask(ctx, id, Asking::File, String::new()),
        "fs.new_folder" => ask(ctx, id, Asking::Folder, String::new()),
        "fs.upload_path" => ask(ctx, id, Asking::Upload, String::new()),
        "fs.name" => {
            let value = event.text().unwrap_or_default().to_string();
            ctx.with_files(id, |f| {
                if let Some((_, typed)) = &mut f.asking {
                    *typed = value;
                }
            });
            ctx.draw_view(id);
        }
        "fs.ok" => answer(ctx, id),
        "fs.cancel" => {
            ctx.with_files(id, |f| f.asking = None);
            ctx.draw_view(id);
        }
        "fs.upload_file" => {
            if let Ok(Some(local)) = ctx.host.pick_file(t!("docker.upload_file"), &[]) {
                ctx.file_job(id, local.display().to_string(), move |cli| {
                    files::copy_in(cli, &place.container(cli)?, &local, &format!("{dir}/"))
                });
            }
        }
        "fs.open" | "fs.edit" => {
            if let Some(entry) = first {
                let path = files::join(&dir, &entry.name);
                if entry.folder {
                    go_to(ctx, id, files::join(&shown, &entry.name));
                } else {
                    ctx.edit_file(place, name, path, root);
                }
            }
        }
        "fs.copy_path" => {
            if let Some(entry) = first {
                ctx.host.copy(files::join(&shown, &entry.name));
            }
        }
        "fs.rename" => {
            if let Some(entry) = first {
                ask(ctx, id, Asking::Rename(entry.name.clone()), entry.name);
            }
        }
        "fs.download" => {
            if let Some(entry) = first {
                let path = files::join(&dir, &entry.name);
                if let Ok(Some(local)) = ctx.host.pick_save_path(entry.name) {
                    ctx.file_job(id, place.shown(&path).to_string(), move |cli| {
                        files::copy_out(cli, &place.container(cli)?, &path, &local)
                    });
                }
            }
        }
        "fs.delete" => {
            let paths: Vec<String> = entries[range]
                .iter()
                .map(|e| files::join(&dir, &e.name))
                .collect();
            if paths.is_empty() {
                return;
            }
            let question = t!(
                "docker.confirm_delete_files",
                count = paths.len(),
                names = paths
                    .iter()
                    .map(|p| place.shown(p))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            if confirm(ctx, question) {
                ctx.file_job(id, t!("docker.delete").to_string(), move |cli| {
                    let container = place.container(cli)?;
                    paths
                        .iter()
                        .try_for_each(|p| files::delete(cli, &container, root, p))
                });
            }
        }
        _ => {}
    }
}

// ── Where the events come from ─────────────────────────────────────────

/// An event of the panel on the left.
pub fn panel_event(ctx: &Ctx, event: &UiEvent) {
    match event.action.as_str() {
        "refresh" => {
            ctx.lock().error.clear();
            ctx.draw_panel();
            ctx.refresh();
        }
        "console" => ctx.host.show_panel(crate::ctx::CONSOLE),
        "filter" => {
            ctx.lock().filter = event.text().unwrap_or_default().to_string();
            ctx.draw_panel();
        }
        "context" => {
            if let Some(name) = event.text() {
                switch_context(ctx, name.to_string());
            }
        }
        "tree" => {
            let Some((row, how)) = event.tree_item() else {
                return;
            };
            // A row that holds others opens; one that doesn't, shows.
            let holds = row.starts_with("g.") || row.starts_with("p:") || row.starts_with("s:");
            if holds || how == TreeAction::Toggle {
                {
                    let mut app = ctx.lock();
                    if !app.open.remove(row) {
                        app.open.insert(row.to_string());
                    }
                }
                ctx.draw_panel();
            } else if let Some(object) = Object::of_row(row) {
                open(ctx, &object, "info");
            }
        }
        "file.service" => {
            let line = event.text().and_then(|l| l.parse().ok());
            let path = ctx.lock().file.as_ref().map(|f| f.path.clone());
            if let (Some(path), Some(line)) = (path, line) {
                open_dialog(ctx, DialogKind::File(path, Some(line)), |_| {});
            }
        }
        action => {
            if let Some(verb) = action.strip_prefix("file.") {
                file_action(ctx, verb, None);
                return;
            }
            match event.menu_item().and_then(Object::of_row) {
                Some(object) => object_action(ctx, action, &object),
                None => {
                    general_action(ctx, action);
                }
            }
        }
    }
}

fn object_action(ctx: &Ctx, action: &str, object: &Object) {
    if action == "open" {
        open(ctx, object, "info");
        return;
    }
    let Some((family, verb)) = action.split_once('.') else {
        return;
    };
    match (family, object) {
        ("ct", Object::Container(id)) => container_action(ctx, verb, id),
        ("im", Object::Image(reference)) => image_action(ctx, verb, reference),
        ("cp", Object::Project(project)) => {
            let compose = compose_of(&ctx.lock().data, project, None);
            compose_action(ctx, verb, compose);
        }
        ("cp", Object::Service(project, service)) => {
            let compose = compose_of(&ctx.lock().data, project, Some(service));
            compose_action(ctx, verb, compose);
        }
        ("vo", Object::Volume(name)) if verb == "remove" => {
            if !confirm(ctx, t!("docker.confirm_remove_volume", name = name)) {
                return;
            }
            // The tab and its helper hold the volume.
            // The job removes the helper itself, before the volume.
            let tab = format!("vol-{name}");
            if ctx.lock().views.remove(&tab).is_some() {
                ctx.host.close_tab(&tab);
            }
            ctx.remove_volume(name.clone());
        }
        ("rg", Object::Registry(host)) => registry_action(ctx, verb, host),
        ("ne", Object::Network(name)) if verb == "remove" => {
            if !confirm(ctx, t!("docker.confirm_remove", name = name)) {
                return;
            }
            ctx.job(
                format!("{} {name}", t!("docker.remove")),
                vec![words(&["network", "rm", name])],
            );
        }
        _ => {}
    }
}

/// An event of a tab or of a dialog.
pub fn view_event(ctx: &Ctx, id: &str, event: &UiEvent) {
    if matches!(ctx.lock().views.get(id), Some(View::Dialog(_))) {
        dialog_event(ctx, id, event);
        return;
    }
    if matches!(ctx.lock().views.get(id), Some(View::Registry(_))) {
        registry_event(ctx, id, event);
        return;
    }
    let action = event.action.as_str();
    if action == "files" || action.starts_with("fs.") {
        files_event(ctx, id, event);
        return;
    }
    match action {
        "detail.tab" => {
            let Some(tab) = event.text() else { return };
            match ctx.lock().views.get_mut(id) {
                Some(View::Container(v)) => {
                    v.tab = tab.to_string();
                    v.table = None;
                }
                Some(View::Image(v)) => {
                    v.tab = tab.to_string();
                    v.table = None;
                }
                Some(View::Object(v)) => v.tab = tab.to_string(),
                _ => return,
            }
            ctx.draw_view(id);
            ctx.load_view(id);
        }
        "detail.refresh" => ctx.load_view(id),
        "ne.pick" => {
            if let Some(View::Object(v)) = ctx.lock().views.get_mut(id) {
                v.picked = event.text().map(str::to_string);
            }
            ctx.draw_view(id);
        }
        "ne.connect" | "ne.disconnect" => {
            let wanted = match ctx.lock().views.get(id) {
                Some(View::Object(v)) => v.picked.clone().map(|c| (v.name.clone(), c)),
                _ => None,
            };
            if let Some((network, container)) = wanted {
                let verb = &action[3..];
                ctx.job(
                    format!("{verb} {container} → {network}"),
                    vec![words(&["network", verb, &network, &container])],
                );
            }
        }
        action => {
            let Some(object) = viewed(ctx, id) else {
                return;
            };
            // An image is known by its id here, by its name in the lists.
            let object = match object {
                Object::Image(image) => Object::Image(
                    ctx.lock()
                        .data
                        .images
                        .iter()
                        .find(|i| i.id == image)
                        .map_or(image, |i| i.reference()),
                ),
                other => other,
            };
            object_action(ctx, action, &object);
        }
    }
}

/// Looks at the engine of another context. The tabs were about this one.
fn switch_context(ctx: &Ctx, name: String) {
    let tabs: Vec<String> = ctx.lock().views.keys().cloned().collect();
    for tab in tabs {
        ctx.close_view(&tab);
    }
    ctx.stop_watching();
    {
        let mut app = ctx.lock();
        app.cli.context = Some(name);
        app.data = Data::default();
        app.loaded = false;
        app.error.clear();
        let json = serde_json::json!({"context": app.cli.context});
        let _ = std::fs::write(app.data_dir.join("settings.json"), json.to_string());
    }
    ctx.draw_panel();
    ctx.refresh();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Container;

    #[test]
    fn the_rows_of_the_tree_say_what_they_are() {
        assert_eq!(
            Object::of_row("s:shop/web"),
            Some(Object::Service("shop".into(), "web".into()))
        );
        assert_eq!(
            Object::of_row("i:registry:5000/a:1"),
            Some(Object::Image("registry:5000/a:1".into()))
        );
        assert_eq!(Object::of_row("g.images"), None);
    }

    #[test]
    fn a_project_is_run_as_its_containers_say() {
        let data = Data {
            containers: vec![Container {
                project: "shop".into(),
                files: "/a/compose.yml,/a/more.yml".into(),
                dir: "/a".into(),
                ..Container::default()
            }],
            ..Data::default()
        };
        let compose = compose_of(&data, "shop", Some("web"));
        assert_eq!(compose.files.len(), 2);
        assert_eq!(compose.dir.as_deref(), Some(Path::new("/a")));
        assert_eq!(compose.service.as_deref(), Some("web"));
    }

    #[test]
    fn the_override_beside_a_compose_file_goes_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("compose.yml");
        let over = dir.path().join("compose.override.yaml");
        std::fs::write(&file, "").unwrap();
        assert_eq!(compose_file(&file, None).files, std::slice::from_ref(&file));
        std::fs::write(&over, "").unwrap();
        assert_eq!(compose_file(&file, None).files, [file, over]);
        let other = dir.path().join("compose.prod.yml");
        assert_eq!(compose_file(&other, None).files.len(), 1);
    }

    #[test]
    fn the_rows_picked_stay_in_the_listing() {
        let s = |a, c| {
            Some(GridSelection {
                anchor: (a, 0),
                cursor: (c, 3),
            })
        };
        assert_eq!(picked(s(3, 1), 10), 1..4);
        assert_eq!(picked(s(8, 20), 10), 8..10);
        assert_eq!(picked(s(12, 20), 10), 10..10);
        assert_eq!(picked(None, 10), 0..0);
    }
}
