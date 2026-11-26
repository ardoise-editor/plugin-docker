//! The Docker plugin of Ardoise: containers, images, volumes, networks and
//! Compose projects in a panel; shells, logs and builds in a console; the
//! files of containers in tabs; the cleanup of the engine; and ▶ in the
//! gutter of Dockerfiles and compose files. It drives the `docker` command.

mod actions;
mod commands;
mod complete;
mod console;
mod ctx;
mod dockerfile;
mod engine;
mod files;
mod registry;
mod snapshot;
mod state;
mod ui;

use std::path::{Path, PathBuf};

use ardoise_plugin::protocol::{
    CompletionParams, Completions, Context, InitializeParams, Settings, UiEvent,
};
use ardoise_plugin::ui::ScreenInput;
use ardoise_plugin::{run, Host, Plugin};
use rust_i18n::t;

use ctx::{Ctx, CONSOLE, PANEL};
use state::{DialogKind, Edit};

rust_i18n::i18n!("locales", fallback = "en");

#[derive(Default)]
struct Docker {
    ctx: Option<Ctx>,
}

impl Docker {
    fn ctx(&mut self, host: &Host) -> Ctx {
        self.ctx
            .get_or_insert_with(|| Ctx::new(host.clone()))
            .clone()
    }
}

impl Plugin for Docker {
    fn initialize(&mut self, host: &Host, params: &InitializeParams) {
        rust_i18n::set_locale(&params.settings.locale);
        let ctx = self.ctx(host);
        {
            let mut app = ctx.lock();
            app.data_dir = params.data_dir.clone();
            let read = |name: &str| std::fs::read_to_string(params.data_dir.join(name)).ok();
            app.cli.context = read("settings.json")
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v["context"].as_str().map(str::to_string));
            app.registries = read("registries.json")
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            app.edits = read("edits.json")
                .and_then(|s| serde_json::from_str::<Vec<(PathBuf, Edit)>>(&s).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|(path, _)| path.is_file())
                .collect();
        }
        ctx.tick();
    }

    fn command(&mut self, host: &Host, command: &str, context: &Context) {
        let ctx = self.ctx(host);
        // A dialog on the engine needs what it lists.
        if matches!(command, "docker.run" | "docker.cleanup") && !ctx.lock().loaded {
            ctx.reload();
        }
        match command {
            "docker.show" => host.show_panel(PANEL),
            "docker.console" => host.show_panel(CONSOLE),
            "docker.run" => actions::open_run(&ctx, ""),
            "docker.pull" => actions::open_dialog(&ctx, DialogKind::Pull, |_| {}),
            "docker.cleanup" => actions::open_cleanup(&ctx),
            "docker.file" => {
                actions::set_file(&ctx, context.file.as_deref());
                let path = ctx.lock().file.as_ref().map(|f| f.path.clone());
                match path {
                    Some(path) => actions::open_dialog(&ctx, DialogKind::File(path, None), |_| {}),
                    None => host.error(t!("docker.not_docker_file")),
                }
            }
            _ => {}
        }
    }

    fn panel_opened(&mut self, host: &Host, panel: &str) {
        let ctx = self.ctx(host);
        match panel {
            PANEL => {
                ctx.lock().panel_shown = true;
                ctx.draw_panel();
                ctx.refresh();
            }
            CONSOLE => {
                ctx.lock().console_shown = true;
                ctx.draw_console();
            }
            tab => {
                if ctx.lock().views.contains_key(tab) {
                    ctx.draw_view(tab);
                } else {
                    actions::restore(&ctx, tab);
                }
            }
        }
    }

    fn panel_closed(&mut self, host: &Host, panel: &str) {
        let ctx = self.ctx(host);
        match panel {
            PANEL => ctx.lock().panel_shown = false,
            CONSOLE => ctx.lock().console_shown = false,
            _ => {}
        }
    }

    fn tab_closed(&mut self, host: &Host, tab: &str) {
        self.ctx(host).forget_view(tab);
    }

    fn ui_event(&mut self, host: &Host, event: &UiEvent) {
        let ctx = self.ctx(host);
        match event.panel.as_str() {
            PANEL => actions::panel_event(&ctx, event),
            CONSOLE => console_event(&ctx, event),
            view => actions::view_event(&ctx, view, event),
        }
    }

    fn active_file_changed(&mut self, host: &Host, path: Option<&Path>) {
        // A tab of the plugin in front is no file: the last one stays.
        if let Some(path) = path {
            actions::set_file(&self.ctx(host), Some(path));
        }
    }

    fn file_saved(&mut self, host: &Host, path: &Path) {
        let ctx = self.ctx(host);
        ctx.write_back(path.to_path_buf());
        if dockerfile::kind(path).is_some() {
            actions::set_file(&ctx, Some(path));
        }
    }

    fn completion(
        &mut self,
        host: &Host,
        params: &CompletionParams,
    ) -> Result<Completions, String> {
        // No panel: a file of the editor.
        let path = Path::new(&params.editor);
        if !params.panel.is_empty() || dockerfile::kind(path) != Some(dockerfile::Kind::Dockerfile)
        {
            return Ok(Completions::default());
        }
        let images: Vec<String> = self
            .ctx(host)
            .lock()
            .data
            .images
            .iter()
            .filter(|i| !i.dangling() && i.tag != "<none>")
            .map(|i| i.reference())
            .collect();
        let folder = path.parent().unwrap_or(Path::new("."));
        Ok(complete::complete(
            &params.text,
            params.offset,
            &images,
            folder,
        ))
    }

    fn run_mark_clicked(&mut self, host: &Host, path: &Path, line: usize) {
        let ctx = self.ctx(host);
        actions::set_file(&ctx, Some(path));
        actions::open_dialog(
            &ctx,
            DialogKind::File(path.to_path_buf(), Some(line)),
            |_| {},
        );
    }

    fn settings_changed(&mut self, host: &Host, settings: &Settings) {
        rust_i18n::set_locale(&settings.locale);
        let ctx = self.ctx(host);
        ctx.draw_all();
        ctx.draw_console();
    }

    fn shutdown(&mut self, host: &Host) {
        let ctx = self.ctx(host);
        ctx.stop_watching();
        ctx.remove_helpers();
        // Dropping the sessions ends what runs in the console.
        ctx.lock().consoles.clear();
    }
}

fn console_event(ctx: &Ctx, event: &UiEvent) {
    if event.action == "screen" {
        let session = {
            let app = ctx.lock();
            app.consoles.get(app.console).map(|t| t.session.clone())
        };
        if let (Some(session), Some(input)) = (session, ScreenInput::from_event(event)) {
            match input {
                ScreenInput::Input { bytes } => session.write(&bytes),
                ScreenInput::Resize { rows, cols } => session.resize(rows, cols),
                ScreenInput::Scroll { lines } => session.scroll(lines),
            }
            console::OUTPUT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        return;
    }
    let Some((what, index)) = event.action.split_once(':') else {
        return;
    };
    let Ok(index) = index.parse::<usize>() else {
        return;
    };
    {
        let mut app = ctx.lock();
        if index >= app.consoles.len() {
            return;
        }
        if what == "tab" {
            app.console = index;
        } else {
            app.consoles.remove(index);
            if app.console > index || app.console >= app.consoles.len() {
                app.console = app.console.saturating_sub(1);
            }
        }
    }
    ctx.draw_console();
}

fn main() -> std::io::Result<()> {
    run(Docker::default())
}
