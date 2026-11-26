//! The plugin at work: what is listed again, drawn again, run in the
//! background or in the console. Cheap to clone, for the threads.
//!
//! The state is never held locked while `docker` runs or while the editor is
//! asked something.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ardoise_plugin::Host;
use rust_i18n::t;
use serde_json::Value;

use crate::commands::words;
use crate::console::{Session, OUTPUT};
use crate::dockerfile::Kind;
use crate::engine::Cli;
use crate::registry::{self, Registry};
use crate::state::*;
use crate::{files, ui};

pub const PANEL: &str = "docker";
pub const CONSOLE: &str = "console";
/// The console is drawn again at most this often while a command prints.
const FRAME: Duration = Duration::from_millis(40);
/// At most one listing a second while the engine changes.
const SETTLE: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct Ctx {
    pub app: Arc<Mutex<App>>,
    pub host: Host,
    /// The engine changed since the last listing.
    pub dirty: Arc<AtomicBool>,
}

impl Ctx {
    pub fn new(host: Host) -> Self {
        Ctx {
            app: Arc::new(Mutex::new(App::new())),
            host,
            dirty: Arc::default(),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, App> {
        self.app.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn cli(&self) -> Cli {
        self.lock().cli.clone()
    }

    // ── Drawing ───────────────────────────────────────────────────────

    pub fn draw_panel(&self) {
        let app = self.lock();
        if app.panel_shown {
            self.host.render(PANEL, ui::panel(&app));
        }
    }

    pub fn draw_console(&self) {
        let app = self.lock();
        if app.console_shown {
            self.host.render(CONSOLE, ui::console(&app));
        }
    }

    /// A tab or a dialog.
    pub fn draw_view(&self, id: &str) {
        let app = self.lock();
        let node = match app.views.get(id) {
            Some(View::Container(view)) => ui::container(&app, view),
            Some(View::Image(view)) => ui::image(&app, view),
            Some(View::Object(view)) => ui::object(&app, view),
            Some(View::Registry(view)) => ui::registry(&app, view),
            Some(View::Dialog(dialog)) => ui::dialog(&app, dialog),
            None => return,
        };
        self.host.render(id, node);
    }

    /// The panel and the tabs (a dialog is drawn again by what it does: a
    /// listing must not disturb what is being typed).
    pub fn draw_all(&self) {
        self.draw_panel();
        let tabs: Vec<String> = self
            .lock()
            .views
            .iter()
            .filter(|(_, v)| !matches!(v, View::Dialog(_)))
            .map(|(k, _)| k.clone())
            .collect();
        for tab in tabs {
            self.draw_view(&tab);
        }
    }

    // ── Listing ───────────────────────────────────────────────────────

    /// Lists the engine again, here. A listing asked after this one wins.
    pub fn reload(&self) {
        let (cli, asked) = {
            let mut app = self.lock();
            app.asked += 1;
            (app.cli.clone(), app.asked)
        };
        let data = Data::load(&cli);
        if data.is_ok() && !self.lock().loaded {
            files::remove_stale(&cli);
        }
        {
            let mut app = self.lock();
            if app.asked != asked {
                return;
            }
            match data {
                Ok(data) => {
                    app.data = data;
                    app.loaded = true;
                    app.error.clear();
                }
                Err(e) => {
                    app.data = Data::default();
                    app.loaded = false;
                    app.error = e;
                }
            }
        }
        self.draw_all();
        self.watch();
    }

    pub fn refresh(&self) {
        let ctx = self.clone();
        std::thread::spawn(move || ctx.reload());
    }

    /// Follows `docker events`, to list again when something changes.
    fn watch(&self) {
        let mut app = self.lock();
        if let Some(watcher) = &mut app.watcher {
            if matches!(watcher.try_wait(), Ok(None)) {
                return;
            }
        }
        if !app.loaded {
            return;
        }
        let spawned = app
            .cli
            .command(&["events", "--format", "{{.Action}}"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else { return };
        if let Some(out) = child.stdout.take() {
            let dirty = self.dirty.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    if changes_the_lists(&line) {
                        dirty.store(true, Ordering::Relaxed);
                    }
                }
            });
        }
        app.watcher = Some(child);
    }

    pub fn stop_watching(&self) {
        if let Some(mut watcher) = self.lock().watcher.take() {
            let _ = watcher.kill();
            let _ = watcher.wait();
        }
    }

    /// The thread that draws the console as the commands print, and lists
    /// again after the engine changed.
    pub fn tick(&self) {
        let ctx = self.clone();
        std::thread::spawn(move || {
            let mut seen = OUTPUT.load(Ordering::Relaxed);
            let mut listed = Instant::now();
            loop {
                std::thread::sleep(FRAME);
                let now = OUTPUT.load(Ordering::Relaxed);
                if now != seen {
                    seen = now;
                    ctx.draw_console();
                }
                if listed.elapsed() >= SETTLE && ctx.dirty.load(Ordering::Relaxed) {
                    let wanted = {
                        let app = ctx.lock();
                        app.panel_shown
                            || !app.views.is_empty()
                            // Its images are offered while one types.
                            || app.file.as_ref().is_some_and(|f| f.kind == Kind::Dockerfile)
                    };
                    if wanted {
                        ctx.dirty.store(false, Ordering::Relaxed);
                        ctx.reload();
                        listed = Instant::now();
                    }
                }
            }
        });
    }

    // ── Running ───────────────────────────────────────────────────────

    /// Runs commands one after the other, off the main thread; says how it
    /// went and lists again. Stops at the first that fails.
    pub fn job(&self, label: String, commands: Vec<Vec<String>>) {
        let ctx = self.clone();
        let cli = {
            let mut app = self.lock();
            app.busy.push(label.clone());
            app.cli.clone()
        };
        self.draw_panel();
        std::thread::spawn(move || {
            let failed = commands.iter().find_map(|args| cli.run_long(args).err());
            ctx.finish(&label, failed.map_or(Ok(()), Err));
        });
    }

    /// Removes a volume, the helper that looked into it first.
    pub fn remove_volume(&self, name: String) {
        let label = format!("{} {name}", t!("docker.remove"));
        let (ctx, cli) = (self.clone(), self.cli());
        self.lock().busy.push(label.clone());
        self.draw_panel();
        std::thread::spawn(move || {
            let removed = files::remove_volume(&cli, &name);
            ctx.finish(&label, removed);
        });
    }

    /// The end of something run in the background (`label` was in `busy`).
    pub fn finish(&self, label: &str, result: Result<(), String>) {
        {
            let mut app = self.lock();
            if let Some(at) = app.busy.iter().position(|b| b == label) {
                app.busy.remove(at);
            }
        }
        match result {
            Ok(()) => self.host.notify(format!("{label} ✓")),
            Err(e) => self.host.error(format!("{label} : {e}")),
        }
        self.reload();
    }

    /// A `docker` command in a tab of the console, in its terminal.
    pub fn console(&self, title: String, args: Vec<String>, dir: Option<&Path>) {
        let args = self.cli().args(&args);
        self.console_program(title, "docker", args, dir);
    }

    /// Several `docker` commands, each if the one before went well.
    pub fn console_chain(&self, title: String, commands: Vec<Vec<String>>, dir: Option<&Path>) {
        let cli = self.cli();
        let script = commands
            .iter()
            .map(|args| {
                let quoted: Vec<String> = cli
                    .args(args)
                    .iter()
                    .map(|a| crate::engine::quote(a))
                    .collect();
                format!("docker {}", quoted.join(" "))
            })
            .collect::<Vec<_>>()
            .join(" && ");
        self.console_program(title, "sh", words(&["-c", &script]), dir);
    }

    fn console_program(&self, title: String, program: &str, args: Vec<String>, dir: Option<&Path>) {
        let dirty = self.dirty.clone();
        let session = Session::spawn(program, &args, dir, move || {
            dirty.store(true, Ordering::Relaxed);
        });
        match session {
            Ok(session) => {
                {
                    let mut app = self.lock();
                    app.consoles.push(ConsoleTab { title, session });
                    app.console = app.consoles.len() - 1;
                }
                self.host.show_panel(CONSOLE);
                self.draw_console();
            }
            Err(e) => self.host.error(e),
        }
    }

    // ── Tabs ──────────────────────────────────────────────────────────

    /// Opens a tab or a dialog on a view and loads what it shows.
    pub fn open_view(&self, id: String, title: String, view: View) {
        let dialog = matches!(view, View::Dialog(_));
        self.lock().views.insert(id.clone(), view);
        let opened = if dialog {
            self.host.open_dialog_with(&id, title, "archive", "Docker")
        } else {
            self.host.open_tab(&id, title)
        };
        if let Err(e) = opened {
            self.lock().views.remove(&id);
            self.host.error(e.message);
            return;
        }
        self.draw_view(&id);
        self.load_view(&id);
    }

    pub fn close_view(&self, id: &str) {
        self.forget_view(id);
        self.host.close_tab(id);
    }

    /// A tab is gone: the helper of the volume it looked into goes too.
    pub fn forget_view(&self, id: &str) {
        let (gone, cli) = {
            let mut app = self.lock();
            (app.views.remove(id), app.cli.clone())
        };
        if let Some(View::Object(view)) = gone {
            if view.kind == ObjectKind::Volume {
                std::thread::spawn(move || files::remove_helper(&cli, &view.name));
            }
        }
    }

    /// The helpers go now (the plugin stops).
    pub fn remove_helpers(&self) {
        let (volumes, cli) = {
            let app = self.lock();
            (app.open_volumes(), app.cli.clone())
        };
        for volume in volumes {
            files::remove_helper(&cli, &volume);
        }
    }

    /// Asks `docker` for what a tab shows, in the background.
    pub fn load_view(&self, id: &str) {
        enum Wanted {
            Container(String, String),
            Image(String, String),
            Object(ObjectKind, String),
            Repositories(String),
        }
        let wanted = match self.lock().views.get(id) {
            Some(View::Container(v)) => Wanted::Container(v.id.clone(), v.tab.clone()),
            Some(View::Image(v)) => Wanted::Image(v.id.clone(), v.tab.clone()),
            Some(View::Object(v)) => Wanted::Object(v.kind, v.name.clone()),
            Some(View::Registry(v)) if v.host != registry::HUB => {
                Wanted::Repositories(v.host.clone())
            }
            _ => return,
        };
        let lists_files = match &wanted {
            Wanted::Container(_, tab) => tab == "files",
            Wanted::Object(ObjectKind::Volume, _) => {
                matches!(self.lock().views.get(id), Some(View::Object(v)) if v.tab == "files")
            }
            _ => false,
        };
        if lists_files {
            self.list_files(id);
            return;
        }
        let (ctx, cli, id) = (self.clone(), self.cli(), id.to_string());
        std::thread::spawn(move || {
            let inspect = |args: &[&str]| -> Result<Value, String> {
                let out = cli.run(args)?;
                let list: Vec<Value> = serde_json::from_str(&out).map_err(|e| e.to_string())?;
                Ok(list.into_iter().next().unwrap_or_default())
            };
            match wanted {
                Wanted::Container(container, tab) => {
                    let result = match tab.as_str() {
                        "top" => cli.run(&["top", &container]).map(|o| (None, Some(top(&o)))),
                        "stats" => cli
                            .run(&["stats", "--no-stream", "--format", STATS, &container])
                            .map(|o| (None, Some(stats(&o)))),
                        _ => {
                            inspect(&["container", "inspect", &container]).map(|v| (Some(v), None))
                        }
                    };
                    if let Some(View::Container(view)) = ctx.lock().views.get_mut(&id) {
                        // The tab may have changed while `docker` answered.
                        if view.tab != tab {
                            return;
                        }
                        match result {
                            Ok((found, table)) => {
                                view.error.clear();
                                view.inspect = found.or(view.inspect.take());
                                view.table = table;
                            }
                            Err(e) => view.error = e,
                        }
                    }
                }
                Wanted::Image(image, tab) => {
                    let result = if tab == "layers" {
                        cli.run(&["history", "--no-trunc", "--format", HISTORY, &image])
                            .map(|o| (None, Some(history(&o))))
                    } else {
                        inspect(&["image", "inspect", &image]).map(|v| (Some(v), None))
                    };
                    if let Some(View::Image(view)) = ctx.lock().views.get_mut(&id) {
                        if view.tab != tab {
                            return;
                        }
                        match result {
                            Ok((found, table)) => {
                                view.error.clear();
                                view.inspect = found.or(view.inspect.take());
                                view.table = table;
                            }
                            Err(e) => view.error = e,
                        }
                    }
                }
                Wanted::Repositories(host) => {
                    let listed = Registry::of_docker(&host).and_then(|r| r.repositories());
                    if let Some(View::Registry(view)) = ctx.lock().views.get_mut(&id) {
                        match listed {
                            Ok(names) => {
                                view.repositories =
                                    names.into_iter().map(|n| (n, String::new())).collect();
                            }
                            Err(e) => view.unlisted = e,
                        }
                    }
                }
                Wanted::Object(kind, name) => {
                    let what = match kind {
                        ObjectKind::Volume => "volume",
                        ObjectKind::Network => "network",
                    };
                    let result = inspect(&[what, "inspect", &name]);
                    if let Some(View::Object(view)) = ctx.lock().views.get_mut(&id) {
                        match result {
                            Ok(found) => view.inspect = Some(found),
                            Err(e) => view.error = e,
                        }
                    }
                }
            }
            ctx.draw_view(&id);
        });
    }

    // ── Registries ────────────────────────────────────────────────────

    /// Something asked of a registry, in the background, for its tab.
    fn ask_registry<T: Send + 'static>(
        &self,
        id: &str,
        ask: impl FnOnce() -> Result<T, String> + Send + 'static,
        then: impl FnOnce(&mut RegistryView, T) + Send + 'static,
    ) {
        if let Some(View::Registry(view)) = self.lock().views.get_mut(id) {
            view.loading = true;
            view.error.clear();
        }
        self.draw_view(id);
        let (ctx, id) = (self.clone(), id.to_string());
        std::thread::spawn(move || {
            let answer = ask();
            if let Some(View::Registry(view)) = ctx.lock().views.get_mut(&id) {
                view.loading = false;
                match answer {
                    Ok(found) => then(view, found),
                    Err(e) => view.error = e,
                }
            }
            ctx.draw_view(&id);
        });
    }

    pub fn load_tags(&self, id: &str, repository: String) {
        let host = match self.lock().views.get(id) {
            Some(View::Registry(view)) => view.host.clone(),
            _ => return,
        };
        let asked = repository.clone();
        self.ask_registry(
            id,
            move || Registry::of_docker(&host)?.tags(&asked),
            move |view, tags| {
                view.repository = Some(repository);
                view.tags = tags;
                view.tag = None;
            },
        );
    }

    /// Looks for repositories on Docker Hub, as `docker search` does.
    pub fn search_hub(&self, id: &str, term: String) {
        let cli = self.cli();
        self.ask_registry(
            id,
            move || {
                let term = crate::engine::name_arg(&term)?;
                let out = cli.run(&["search", "--limit", "50", "--format", "{{json .}}", term])?;
                crate::engine::parse_lines::<Value>(&out)
            },
            |view, found| {
                view.repositories = found
                    .iter()
                    .map(|f| {
                        let said = |key: &str| f[key].as_str().unwrap_or_default().to_string();
                        (
                            said("Name"),
                            format!("★ {}  {}", said("StarCount"), said("Description")),
                        )
                    })
                    .collect();
            },
        );
    }

    // ── Files of a container or of a volume ───────────────────────────

    /// What the files tab of a view looks at: (where, the folder as shown,
    /// as root).
    pub fn files_of(&self, id: &str) -> Option<(Place, String, bool)> {
        match self.lock().views.get(id) {
            Some(View::Container(v)) => Some((
                Place::Container(v.id.clone()),
                v.files.path.clone(),
                v.files.root,
            )),
            // The helper of a volume runs as root.
            Some(View::Object(v)) if v.kind == ObjectKind::Volume => {
                Some((Place::Volume(v.name.clone()), v.files.path.clone(), false))
            }
            _ => None,
        }
    }

    pub fn with_files(&self, id: &str, change: impl FnOnce(&mut FilesView)) {
        match self.lock().views.get_mut(id) {
            Some(View::Container(view)) => change(&mut view.files),
            Some(View::Object(view)) => change(&mut view.files),
            _ => {}
        }
    }

    pub fn list_files(&self, id: &str) {
        let Some((place, shown, root)) = self.files_of(id) else {
            return;
        };
        // A container that doesn't run can't list: the tab says so.
        if let Place::Container(container) = &place {
            let running = self
                .lock()
                .data
                .container(container)
                .is_some_and(|c| c.running());
            if !running {
                self.with_files(id, |f| {
                    f.entries.clear();
                    f.loading = false;
                });
                self.draw_view(id);
                return;
            }
        }
        self.with_files(id, |f| f.loading = true);
        self.draw_view(id);
        let (ctx, cli, id) = (self.clone(), self.cli(), id.to_string());
        std::thread::spawn(move || {
            let listed = place
                .container(&cli)
                .and_then(|c| files::list(&cli, &c, root, &place.real(&shown)));
            if let Place::Volume(volume) = &place {
                // The tab was closed while its helper was made.
                if !ctx.lock().open_volumes().contains(volume) {
                    files::remove_helper(&cli, volume);
                    return;
                }
            }
            ctx.with_files(&id, |f| {
                if f.path != shown || ctx_root_changed(f, &place, root) {
                    return;
                }
                f.loading = false;
                f.selection = None;
                match listed {
                    Ok(entries) => {
                        f.entries = entries;
                        f.error.clear();
                    }
                    Err(e) => {
                        f.entries.clear();
                        f.error = e;
                    }
                }
            });
            ctx.draw_view(&id);
        });
    }

    /// Something done to the files of a container, then the folder again.
    pub fn file_job(
        &self,
        id: &str,
        label: String,
        work: impl FnOnce(&Cli) -> Result<(), String> + Send + 'static,
    ) {
        let (ctx, cli, id) = (self.clone(), self.cli(), id.to_string());
        self.lock().busy.push(label.clone());
        std::thread::spawn(move || {
            let result = work(&cli);
            ctx.finish(&label, result);
            ctx.list_files(&id);
        });
    }

    /// Opens a copy of a file of a container (or of a volume) in the
    /// editor; saving the copy writes the file. `name` is that of the
    /// container or of the volume.
    pub fn edit_file(&self, place: Place, name: String, path: String, root: bool) {
        let (ctx, cli) = (self.clone(), self.cli());
        let (folder_name, volume) = match &place {
            Place::Container(_) => (name.clone(), None),
            Place::Volume(volume) => (format!("volume-{volume}"), Some(volume.clone())),
        };
        let folder = self
            .lock()
            .data_dir
            .join("edit")
            .join(cli.context.as_deref().unwrap_or("current"));
        let local = files::local_copy(&folder, &folder_name, &path);
        std::thread::spawn(move || {
            let shown = place.shown(&path).to_string();
            let copied = place
                .container(&cli)
                .and_then(|c| copy_for_editing(&cli, &c, &path, &local));
            if let Err(e) = copied {
                ctx.host.error(format!("{shown} : {e}"));
                return;
            }
            {
                let mut app = ctx.lock();
                app.edits.insert(
                    local.clone(),
                    Edit {
                        context: cli.context.clone(),
                        // The name lasts longer than the id: a container
                        // made again keeps it.
                        container: if volume.is_some() {
                            String::new()
                        } else {
                            name.clone()
                        },
                        volume,
                        path: path.clone(),
                        root,
                    },
                );
                save_edits(&app);
            }
            if let Err(e) = ctx.host.open_file(&local, None) {
                ctx.host.error(e.message);
                return;
            }
            ctx.host.set_origin(&local, Some(format!("{name}:{shown}")));
        });
    }

    /// A copy was saved in the editor: the container or the volume gets it.
    pub fn write_back(&self, local: PathBuf) {
        let Some(edit) = self.lock().edits.get(&local).cloned() else {
            return;
        };
        let ctx = self.clone();
        let cli = Cli {
            context: edit.context.clone(),
        };
        std::thread::spawn(move || {
            let place = match &edit.volume {
                Some(volume) => Place::Volume(volume.clone()),
                None => Place::Container(edit.container.clone()),
            };
            let name = edit.volume.as_ref().unwrap_or(&edit.container);
            let what = format!("{name}:{}", place.shown(&edit.path));
            let written = place
                .container(&cli)
                .and_then(|c| files::write_back(&cli, &c, edit.root, &local, &edit.path));
            if let Some(volume) = &edit.volume {
                if !ctx.lock().open_volumes().contains(volume) {
                    files::remove_helper(&cli, volume);
                }
            }
            match written {
                Ok(()) => ctx.host.notify(t!("docker.saved_to", path = what)),
                Err(e) => ctx
                    .host
                    .error(t!("docker.not_saved_to", path = what, error = e)),
            }
        });
    }
}

/// Whether "as root" was ticked or not since a listing of a container was
/// asked.
fn ctx_root_changed(files: &FilesView, place: &Place, asked: bool) -> bool {
    matches!(place, Place::Container(_)) && files.root != asked
}

/// The biggest file the editor opens from a container.
const MAX_EDIT: u64 = 16 * 1024 * 1024;

fn copy_for_editing(cli: &Cli, container: &str, path: &str, local: &Path) -> Result<(), String> {
    let parent = local.parent().ok_or("no folder")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // The files of a container may hold secrets: for this user only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let _ = std::fs::remove_file(local);
    files::copy_out(cli, container, path, local)?;
    let meta = std::fs::metadata(local).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > MAX_EDIT {
        let _ = std::fs::remove_file(local);
        return Err(t!("docker.not_editable").to_string());
    }
    // A file that is read only in the container is still edited here.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() | 0o600;
        let _ = std::fs::set_permissions(local, std::fs::Permissions::from_mode(mode));
    }
    Ok(())
}

fn save_edits(app: &App) {
    let list: Vec<(&PathBuf, &Edit)> = app.edits.iter().collect();
    if let Ok(json) = serde_json::to_string(&list) {
        let _ = std::fs::write(app.data_dir.join("edits.json"), json);
    }
}

/// Whether an event of the engine changes what the lists show (each `exec`
/// and each copy of a file is one too, the plugin's own among them).
fn changes_the_lists(action: &str) -> bool {
    !(action.starts_with("exec_")
        || matches!(action, "top" | "archive-path" | "extract-to-dir" | "resize"))
}

const STATS: &str =
    "{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}\t{{.NetIO}}\t{{.BlockIO}}\t{{.PIDs}}";
const HISTORY: &str = "{{.CreatedSince}}\t{{.Size}}\t{{.CreatedBy}}";

type Table = (Vec<String>, Vec<Vec<String>>);

fn tabbed(out: &str, titles: &[&str]) -> Table {
    (
        titles.iter().map(|k| t!(*k).into_owned()).collect(),
        out.lines()
            .map(|l| l.splitn(titles.len(), '\t').map(str::to_string).collect())
            .collect(),
    )
}

fn stats(out: &str) -> Table {
    tabbed(
        out,
        &[
            "docker.cpu",
            "docker.memory",
            "docker.memory_percent",
            "docker.network_io",
            "docker.block_io",
            "docker.pids",
        ],
    )
}

fn history(out: &str) -> Table {
    tabbed(out, &["docker.created", "docker.size", "docker.command"])
}

/// The columns of `docker top`; the last one, the command, takes the rest.
pub fn top(out: &str) -> Table {
    let mut lines = out.lines();
    let titles: Vec<String> = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let rows = lines
        .map(|line| {
            let mut cells = Vec::new();
            let mut rest = line.trim_start();
            while cells.len() + 1 < titles.len() {
                let (cell, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                cells.push(cell.to_string());
                rest = after.trim_start();
            }
            cells.push(rest.to_string());
            cells
        })
        .collect();
    (titles, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_processes_keep_their_whole_command() {
        let (titles, rows) = top("UID   PID   CMD\nroot  12    sleep  60 now\n");
        assert_eq!(titles, ["UID", "PID", "CMD"]);
        assert_eq!(rows, [["root", "12", "sleep  60 now"]]);
    }

    #[test]
    fn what_the_plugin_runs_itself_does_not_list_again() {
        assert!(!changes_the_lists("exec_start: sh -c x"));
        assert!(!changes_the_lists("archive-path"));
        assert!(changes_the_lists("die"));
        assert!(changes_the_lists("health_status: healthy"));
    }
}
