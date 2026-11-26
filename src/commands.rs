//! The `docker` commands made from what the user chose: the arguments only,
//! so that they can be checked without an engine.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::engine::{name_arg, split_words};

pub fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|w| w.to_string()).collect()
}

/// A shell in a container: `bash` if it has one, else `sh`.
pub fn shell(container: &str, root: bool) -> Vec<String> {
    let mut args = words(&["exec", "-it"]);
    if root {
        args.extend(words(&["-u", "0"]));
    }
    args.extend(words(&[
        container,
        "sh",
        "-c",
        "command -v bash >/dev/null 2>&1 && exec bash || exec sh",
    ]));
    args
}

/// The fields and the boxes of a dialog.
#[derive(Debug, Clone, Default)]
pub struct Form {
    pub fields: BTreeMap<String, String>,
    pub flags: BTreeSet<String>,
}

impl Form {
    pub fn get(&self, id: &str) -> &str {
        self.fields.get(id).map_or("", |v| v.trim())
    }

    pub fn on(&self, id: &str) -> bool {
        self.flags.contains(id)
    }

    pub fn set(&mut self, id: &str, value: impl Into<String>) {
        self.fields.insert(id.to_string(), value.into());
    }

    pub fn flag(&mut self, id: &str, on: bool) {
        if on {
            self.flags.insert(id.to_string());
        } else {
            self.flags.remove(id);
        }
    }
}

/// `docker run` (or `create`) from the dialog of a new container.
pub fn run_container(form: &Form, create: bool) -> Result<Vec<String>, String> {
    let image = name_arg(form.get("image"))?.to_string();
    let mut args = words(&[if create { "create" } else { "run" }]);
    let interactive = form.on("interactive");
    if interactive {
        args.push("-it".into());
    } else if !create && form.on("detach") {
        args.push("-d".into());
    }
    if form.on("rm") {
        args.push("--rm".into());
    }
    if form.on("publish_all") {
        args.push("-P".into());
    }
    for (field, flag) in [
        ("name", "--name"),
        ("network", "--network"),
        ("restart", "--restart"),
        ("workdir", "--workdir"),
        ("user", "--user"),
    ] {
        let value = form.get(field);
        if !value.is_empty() {
            args.push(flag.into());
            args.push(name_arg(value)?.to_string());
        }
    }
    for (field, flag) in [("ports", "-p"), ("volumes", "-v"), ("env", "-e")] {
        for value in split_words(form.get(field))? {
            args.push(flag.into());
            args.push(value);
        }
    }
    let env_file = form.get("env_file");
    if !env_file.is_empty() {
        args.push("--env-file".into());
        args.push(env_file.to_string());
    }
    args.extend(split_words(form.get("options"))?);
    args.push(image);
    args.extend(split_words(form.get("command"))?);
    Ok(args)
}

/// `docker build` from its dialog.
pub fn build(form: &Form) -> Result<Vec<String>, String> {
    let mut args = words(&["build"]);
    let file = form.get("file");
    if !file.is_empty() {
        args.push("-f".into());
        args.push(file.to_string());
    }
    for tag in split_words(form.get("tag"))? {
        args.push("-t".into());
        args.push(tag);
    }
    let target = form.get("target");
    if !target.is_empty() {
        args.push("--target".into());
        args.push(name_arg(target)?.to_string());
    }
    let platform = form.get("platform");
    if !platform.is_empty() {
        args.push("--platform".into());
        args.push(name_arg(platform)?.to_string());
    }
    for value in split_words(form.get("build_args"))? {
        args.push("--build-arg".into());
        args.push(value);
    }
    if form.on("no_cache") {
        args.push("--no-cache".into());
    }
    if form.on("pull") {
        args.push("--pull".into());
    }
    args.extend(split_words(form.get("options"))?);
    args.push(name_arg(form.get("context"))?.to_string());
    Ok(args)
}

/// A Compose project, or one of its services.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compose {
    pub files: Vec<PathBuf>,
    pub project: Option<String>,
    pub dir: Option<PathBuf>,
    pub service: Option<String>,
}

impl Compose {
    /// The verbs `docker compose` has here, with what comes before them.
    pub fn args(&self, verb: &str) -> Option<Vec<String>> {
        let tail: &[&str] = match verb {
            "up" => &["up", "-d"],
            "upbuild" => &["up", "-d", "--build"],
            "recreate" => &["up", "-d", "--force-recreate"],
            "down" => &["down"],
            "downv" => &["down", "--volumes"],
            "downall" => &["down", "--volumes", "--rmi", "local", "--remove-orphans"],
            "start" => &["start"],
            "stop" => &["stop"],
            "restart" => &["restart"],
            "pause" => &["pause"],
            "unpause" => &["unpause"],
            "logs" => &["logs", "-f", "--tail", "500"],
            "pull" => &["pull"],
            "build" => &["build"],
            "ps" => &["ps", "-a"],
            "config" => &["config"],
            _ => return None,
        };
        let mut args = self.base();
        args.extend(words(tail));
        // `config` is about the whole file.
        if let Some(service) = self.service.as_ref().filter(|_| verb != "config") {
            args.push(service.clone());
        }
        Some(args)
    }

    /// `compose` with the project and its files. A project whose files are
    /// gone is still known by its name.
    pub fn base(&self) -> Vec<String> {
        let mut args = words(&["compose"]);
        if let Some(project) = &self.project {
            args.extend(words(&["-p", project]));
        }
        if !self.files.is_empty() && self.files.iter().all(|f| f.is_file()) {
            if let Some(dir) = &self.dir {
                args.push("--project-directory".into());
                args.push(dir.to_string_lossy().into_owned());
            }
            for file in &self.files {
                args.push("-f".into());
                args.push(file.to_string_lossy().into_owned());
            }
        }
        args
    }
}

/// The boxes of the cleanup, in the order they run: containers first (they
/// hold the rest).
pub const CLEANUPS: &[&str] = &[
    "stopped",
    "dangling",
    "all_images",
    "networks",
    "anonymous_volumes",
    "all_volumes",
    "cache",
    "all_cache",
];

/// The `prune` commands of the boxes ticked. `until` (`24h`) spares what is
/// newer; the volumes have no such filter, so they are left out of it.
pub fn cleanup(form: &Form) -> Result<Vec<Vec<String>>, String> {
    let until = form.get("until");
    if !until.is_empty() {
        name_arg(until)?;
    }
    let filtered = |list: &[&str]| {
        let mut args = words(list);
        if !until.is_empty() {
            args.push("--filter".into());
            args.push(format!("until={until}"));
        }
        args
    };
    let mut commands = Vec::new();
    if form.on("stopped") {
        commands.push(filtered(&["container", "prune", "-f"]));
    }
    if form.on("all_images") {
        commands.push(filtered(&["image", "prune", "-f", "-a"]));
    } else if form.on("dangling") {
        commands.push(filtered(&["image", "prune", "-f"]));
    }
    if form.on("networks") {
        commands.push(filtered(&["network", "prune", "-f"]));
    }
    if form.on("all_volumes") {
        commands.push(words(&["volume", "prune", "-f", "-a"]));
    } else if form.on("anonymous_volumes") {
        commands.push(words(&["volume", "prune", "-f"]));
    }
    if form.on("all_cache") {
        commands.push(filtered(&["builder", "prune", "-f", "-a"]));
    } else if form.on("cache") {
        commands.push(filtered(&["builder", "prune", "-f"]));
    }
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(fields: &[(&str, &str)], flags: &[&str]) -> Form {
        let mut f = Form::default();
        for (k, v) in fields {
            f.set(k, *v);
        }
        for flag in flags {
            f.flag(flag, true);
        }
        f
    }

    #[test]
    fn a_new_container_takes_what_the_dialog_says() {
        let f = form(
            &[
                ("image", "nginx:1"),
                ("name", "web"),
                ("ports", "8080:80 443:443"),
                ("env", "A=1 \"B=two words\""),
                ("command", "nginx -g 'daemon off;'"),
            ],
            &["detach", "rm"],
        );
        assert_eq!(
            run_container(&f, false).unwrap().join("|"),
            "run|-d|--rm|--name|web|-p|8080:80|-p|443:443|-e|A=1|-e|B=two words|nginx:1|nginx|-g|daemon off;"
        );
        assert_eq!(run_container(&f, true).unwrap()[..2], ["create", "--rm"]);
        assert!(run_container(&form(&[("image", "--privileged")], &[]), false).is_err());
        assert!(run_container(&Form::default(), false).is_err());
        assert!(run_container(&form(&[("image", "a"), ("name", "-x")], &[]), false).is_err());
    }

    #[test]
    fn a_build_takes_its_context_last() {
        let f = form(
            &[
                ("file", "/p/Dockerfile"),
                ("tag", "a:1"),
                ("target", "prod"),
                ("context", "/p"),
            ],
            &["no_cache"],
        );
        assert_eq!(
            build(&f).unwrap(),
            [
                "build",
                "-f",
                "/p/Dockerfile",
                "-t",
                "a:1",
                "--target",
                "prod",
                "--no-cache",
                "/p"
            ]
        );
        assert!(build(&Form::default()).is_err());
    }

    #[test]
    fn compose_is_told_the_project_its_files_and_the_service() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("compose.yml");
        std::fs::write(&file, "services:\n").unwrap();
        let mut c = Compose {
            files: vec![file.clone()],
            service: Some("web".into()),
            ..Compose::default()
        };
        let file_text = file.to_string_lossy().into_owned();
        assert_eq!(
            c.args("up").unwrap(),
            ["compose", "-f", &file_text, "up", "-d", "web"]
        );
        assert_eq!(c.args("config").unwrap().last().unwrap(), "config");
        assert!(c.args("nothing").is_none());
        c.project = Some("p".into());
        c.files = vec![PathBuf::from("/gone/compose.yml")];
        assert_eq!(
            c.args("stop").unwrap(),
            ["compose", "-p", "p", "stop", "web"]
        );
    }

    #[test]
    fn the_cleanup_runs_only_what_is_ticked_containers_first() {
        assert!(cleanup(&Form::default()).unwrap().is_empty());
        let f = form(
            &[("until", "24h")],
            &["cache", "anonymous_volumes", "dangling", "stopped"],
        );
        let lines: Vec<String> = cleanup(&f).unwrap().iter().map(|c| c.join(" ")).collect();
        assert_eq!(
            lines,
            [
                "container prune -f --filter until=24h",
                "image prune -f --filter until=24h",
                "volume prune -f",
                "builder prune -f --filter until=24h",
            ]
        );
        let all = form(&[], CLEANUPS);
        let lines: Vec<String> = cleanup(&all).unwrap().iter().map(|c| c.join(" ")).collect();
        assert_eq!(lines.len(), 5, "the wider box takes the narrower one in");
        assert!(lines.contains(&"volume prune -f -a".to_string()));
        assert!(cleanup(&form(&[("until", "-x")], &["stopped"])).is_err());
    }
}
