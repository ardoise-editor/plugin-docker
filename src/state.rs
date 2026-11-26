//! What the plugin knows: the engine as last listed, the tabs and dialogs
//! open, the commands of the console.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Child;
use std::sync::Arc;

use ardoise_plugin::ui::GridSelection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::commands::Form;
use crate::console::Session;
use crate::dockerfile::{Kind, Target};
use crate::engine::{Cli, Container, DockerContext, Image, Network, Volume};
use crate::files::Entry;

/// What the engine holds.
#[derive(Debug, Default)]
pub struct Data {
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    pub networks: Vec<Network>,
    pub contexts: Vec<DockerContext>,
    /// The registries `docker` is logged in to.
    pub logged_in: BTreeSet<String>,
}

impl Data {
    pub fn load(cli: &Cli) -> Result<Data, String> {
        let mut data = Data {
            containers: cli.containers()?,
            images: cli.images()?,
            volumes: cli.volumes()?,
            networks: cli.networks()?,
            contexts: cli.contexts().unwrap_or_default(),
            logged_in: crate::registry::logged_in().into_keys().collect(),
        };
        data.containers.sort_by(|a, b| a.name.cmp(&b.name));
        data.images.sort_by_key(|i| (i.dangling(), i.reference()));
        data.volumes.sort_by(|a, b| a.name.cmp(&b.name));
        data.networks.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(data)
    }

    /// A container by its id (or the start of it) or its name.
    pub fn container(&self, key: &str) -> Option<&Container> {
        self.containers
            .iter()
            .find(|c| c.name == key || (key.len() >= 12 && c.id.starts_with(key)))
    }

    pub fn projects(&self) -> BTreeMap<&str, Vec<&Container>> {
        let mut projects: BTreeMap<&str, Vec<&Container>> = BTreeMap::new();
        for c in self.containers.iter().filter(|c| !c.project.is_empty()) {
            projects.entry(&c.project).or_default().push(c);
        }
        projects
    }
}

#[derive(Debug, Default)]
pub struct FilesView {
    pub path: String,
    /// What the field of the path holds, while typed in.
    pub typed: Option<String>,
    pub entries: Vec<Entry>,
    pub selection: Option<GridSelection>,
    /// A name being asked: what for, and what is typed.
    pub asking: Option<(Asking, String)>,
    /// Act as root.
    pub root: bool,
    pub error: String,
    pub loading: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asking {
    File,
    Folder,
    Rename(String),
    /// A path of this machine to copy into the folder.
    Upload,
}

#[derive(Debug, Default)]
pub struct ContainerView {
    pub id: String,
    pub tab: String,
    pub inspect: Option<Value>,
    /// `docker top`, `docker stats`: the titles, then a row a line.
    pub table: Option<(Vec<String>, Vec<Vec<String>>)>,
    pub files: FilesView,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct ImageView {
    pub id: String,
    pub tab: String,
    pub inspect: Option<Value>,
    pub table: Option<(Vec<String>, Vec<Vec<String>>)>,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Volume,
    Network,
}

#[derive(Debug)]
pub struct ObjectView {
    pub kind: ObjectKind,
    pub name: String,
    /// `files` or `inspect`, for a volume.
    pub tab: String,
    pub files: FilesView,
    pub inspect: Option<Value>,
    /// The container picked to be connected to a network.
    pub picked: Option<String>,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogKind {
    Run,
    Pull,
    Build,
    Volume,
    Network,
    Tag(String),
    Exec(String),
    Commit(String),
    Scale(crate::commands::Compose),
    /// What a file can run: the target clicked in its gutter, or all.
    File(PathBuf, Option<usize>),
    Cleanup,
    Login,
    /// An image to send to a registry.
    Push(String),
}

/// A tab on a registry.
#[derive(Debug, Default)]
pub struct RegistryView {
    pub host: String,
    /// What is typed: a repository, and what to look for on Docker Hub.
    pub typed: String,
    pub search: String,
    /// The repositories listed or found: name, then what is said of it.
    pub repositories: Vec<(String, String)>,
    /// The repository whose tags are shown.
    pub repository: Option<String>,
    pub tags: Vec<String>,
    pub tag: Option<usize>,
    /// Why the registry doesn't list its repositories.
    pub unlisted: String,
    pub error: String,
    pub loading: bool,
}

/// What the cleanup shows before it runs.
#[derive(Debug, Default)]
pub struct Preview {
    pub usage: Vec<crate::engine::Usage>,
    /// The names of what each box would remove.
    pub items: BTreeMap<&'static str, Vec<String>>,
}

#[derive(Debug)]
pub struct Dialog {
    pub kind: DialogKind,
    pub form: Form,
    pub error: String,
    pub busy: bool,
    pub preview: Option<Preview>,
    /// What the cleanup said.
    pub report: String,
}

impl Dialog {
    pub fn new(kind: DialogKind) -> Self {
        Dialog {
            kind,
            form: Form::default(),
            error: String::new(),
            busy: false,
            preview: None,
            report: String::new(),
        }
    }
}

#[derive(Debug)]
pub enum View {
    Container(ContainerView),
    Image(ImageView),
    Object(ObjectView),
    Registry(RegistryView),
    Dialog(Box<Dialog>),
}

pub struct ConsoleTab {
    pub title: String,
    pub session: Arc<Session>,
}

/// A copy of a file of a container open in the editor: saving it writes it
/// back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub context: Option<String>,
    /// The name of the container (empty for a file of a volume).
    pub container: String,
    #[serde(default)]
    pub volume: Option<String>,
    pub path: String,
    pub root: bool,
}

/// Where the files listed in a tab are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    Container(String),
    /// Looked into through a container of the plugin's own.
    Volume(String),
}

impl Place {
    /// A path as shown, as it is in the container.
    pub fn real(&self, shown: &str) -> String {
        match self {
            Place::Container(_) => shown.to_string(),
            Place::Volume(_) => crate::files::in_volume(shown),
        }
    }

    /// A path of the container, as it is shown.
    pub fn shown<'a>(&self, real: &'a str) -> &'a str {
        match self {
            Place::Container(_) => real,
            Place::Volume(_) => match real.strip_prefix(crate::files::VOLUME_ROOT) {
                Some("") => "/",
                Some(rest) => rest,
                None => real,
            },
        }
    }

    /// The container to run in; the helper of a volume is made if needed.
    pub fn container(&self, cli: &Cli) -> Result<String, String> {
        match self {
            Place::Container(id) => Ok(id.clone()),
            Place::Volume(name) => crate::files::ensure_helper(cli, name),
        }
    }
}

/// The Docker file in front.
#[derive(Debug, Clone)]
pub struct DockerFile {
    pub path: PathBuf,
    pub kind: Kind,
    pub targets: Vec<Target>,
}

#[derive(Default)]
pub struct App {
    pub cli: Cli,
    pub data: Data,
    pub loaded: bool,
    pub error: String,
    pub filter: String,
    /// The rows of the tree that are open.
    pub open: BTreeSet<String>,
    /// What runs in the background, said at the top of the panel.
    pub busy: Vec<String>,
    pub panel_shown: bool,
    pub console_shown: bool,
    pub consoles: Vec<ConsoleTab>,
    pub console: usize,
    pub views: BTreeMap<String, View>,
    pub file: Option<DockerFile>,
    pub edits: BTreeMap<PathBuf, Edit>,
    /// The registries added to be looked into, without logging in.
    pub registries: BTreeSet<String>,
    pub data_dir: PathBuf,
    /// `docker events`, which says when to list again.
    pub watcher: Option<Child>,
    /// Counts the listings asked: only the last one is kept.
    pub asked: u64,
}

impl App {
    /// The registries shown: Docker Hub, those `docker` is logged in to,
    /// and those added.
    pub fn registry_hosts(&self) -> BTreeSet<String> {
        std::iter::once(crate::registry::HUB.to_string())
            .chain(self.data.logged_in.iter().cloned())
            .chain(self.registries.iter().cloned())
            .collect()
    }

    /// The volumes a tab looks into.
    pub fn open_volumes(&self) -> Vec<String> {
        self.views
            .values()
            .filter_map(|v| match v {
                View::Object(o) if o.kind == ObjectKind::Volume => Some(o.name.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn new() -> Self {
        App {
            open: ["g.compose", "g.containers"]
                .into_iter()
                .map(String::from)
                .collect(),
            ..App::default()
        }
    }
}
