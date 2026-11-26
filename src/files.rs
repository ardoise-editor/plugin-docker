//! The files of a container: listed and changed with `sh` in it (it must
//! run), copied in and out with `docker cp`.
//!
//! A name of a file of a container is anything: it is only ever given as an
//! argument of its own, never written in the text of a script.

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use crate::engine::{Cli, LONG};

/// What the user may do as: the user of the image, or root.
fn exec_args(container: &str, root: bool, input: bool) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    if input {
        args.push("-i".into());
    }
    if root {
        args.extend(["-u".into(), "0".into()]);
    }
    args.push(container.into());
    args
}

/// Runs a script of `sh` in the container, `values` being `$1`, `$2`…
fn sh(
    cli: &Cli,
    container: &str,
    root: bool,
    script: &str,
    values: &[&str],
    input: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let mut args = exec_args(container, root, input.is_some());
    args.extend(["sh", "-c", script, "sh"].map(String::from));
    args.extend(values.iter().map(|v| v.to_string()));
    cli.run_with(&args, input, LONG)
}

/// The label of the containers the plugin makes for itself: they are not
/// listed.
pub const HELPER_LABEL: &str = "dev.ardoise.helper";
/// What looks into a volume: a small image with `sh` and `stat`.
const HELPER_IMAGE: &str = "busybox:stable";
/// Where the volume is in it.
pub const VOLUME_ROOT: &str = "/volume";
/// A helper left behind (the editor was killed) ends by itself.
const HELPER_LIFE: &str = "3600";

/// Helpers are made and removed one at a time: a volume must not be removed
/// while one is being made for it.
static HELPERS: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    HELPERS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The name of the container that looks into a volume, for this run of the
/// plugin.
pub fn helper_name(volume: &str) -> String {
    format!("ardoise-volume-{}-{volume}", std::process::id())
}

/// A path as shown (from the top of the volume) as it is in the helper.
pub fn in_volume(shown: &str) -> String {
    normalize(&format!("{VOLUME_ROOT}/{}", normalize(shown)))
}

/// The name of this machine: what its helpers are labelled with, since an
/// engine may be shared by several.
fn machine() -> String {
    let mut name = [0u8; 256];
    // SAFETY: `gethostname` writes at most the length given.
    let ok = unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len() - 1) } == 0;
    let end = name.iter().position(|b| *b == 0).unwrap_or(0);
    if ok && end > 0 {
        String::from_utf8_lossy(&name[..end]).into_owned()
    } else {
        "unknown".into()
    }
}

/// The process of the plugin that made a helper, from its name.
fn maker(name: &str) -> Option<i32> {
    name.strip_prefix("ardoise-volume-")?
        .split('-')
        .next()?
        .parse()
        .ok()
        .filter(|pid| *pid > 0)
}

/// Removes the helpers left by a plugin of this machine that was killed
/// before it could.
pub fn remove_stale(cli: &Cli) {
    let filter = format!("label={HELPER_LABEL}={}", machine());
    let listed = cli.run(&["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"]);
    for name in listed.unwrap_or_default().lines() {
        // SAFETY: signal 0 only asks whether the process is there.
        let gone = maker(name).is_some_and(|pid| {
            (unsafe { libc::kill(pid, 0) }) != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        });
        if gone {
            let _ = cli.run(&["rm", "-f", name]);
        }
    }
}

/// Makes the helper of a volume, unless it runs; its name. The volume must
/// be there: `docker run -v` would make an empty one.
pub fn ensure_helper(cli: &Cli, volume: &str) -> Result<String, String> {
    let _held = one_at_a_time();
    let name = helper_name(volume);
    let running = || {
        cli.run(&[
            "container",
            "inspect",
            "--format",
            "{{.State.Running}}",
            &name,
        ])
        .is_ok_and(|out| out.trim() == "true")
    };
    if running() {
        return Ok(name);
    }
    cli.run(&["volume", "inspect", "--format", "{{.Name}}", volume])?;
    let mount = format!("{volume}:{VOLUME_ROOT}");
    let label = format!("{HELPER_LABEL}={}", machine());
    let made = cli.run_long(&[
        "run",
        "-d",
        "--rm",
        "--init",
        "--network",
        "none",
        "--name",
        &name,
        "--label",
        &label,
        "-v",
        &mount,
        HELPER_IMAGE,
        "sleep",
        HELPER_LIFE,
    ]);
    made.map(|_| name)
}

/// Removes the helper of a volume, if there is one. Without `-v`: no volume
/// goes with it.
///
/// Returns once it is gone: one being removed still holds the volume.
pub fn remove_helper(cli: &Cli, volume: &str) {
    let _held = one_at_a_time();
    remove_held(cli, volume);
}

/// Removes a volume, its helper first, none being made meanwhile.
pub fn remove_volume(cli: &Cli, volume: &str) -> Result<(), String> {
    let _held = one_at_a_time();
    remove_held(cli, volume);
    cli.run_long(&["volume", "rm", volume]).map(|_| ())
}

fn remove_held(cli: &Cli, volume: &str) {
    let name = helper_name(volume);
    let _ = cli.run(&["rm", "-f", &name]);
    for _ in 0..100 {
        if cli
            .run(&["container", "inspect", "--format", "x", &name])
            .is_err()
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub folder: bool,
    pub link: bool,
    pub size: u64,
    pub mode: String,
    pub owner: String,
}

/// A record a file: what it is, its size, rights and owner, then its name,
/// ended by a NUL (a name may hold a new line). `stat` of GNU and of
/// BusyBox both know these formats.
const LIST: &str = r#"cd -- "$1" || exit 1
for f in * .[!.]* ..?*; do
  [ -e "$f" ] || [ -L "$f" ] || continue
  k=f; [ -d "$f" ] && k=d
  l=-; [ -L "$f" ] && l=l
  s=$(stat -c '%s|%A|%U' -- "$f" 2>/dev/null)
  printf '%s|%s|%s|%s\000' "$k" "$l" "${s:-0|?|?}" "$f"
done"#;

pub fn list(cli: &Cli, container: &str, root: bool, dir: &str) -> Result<Vec<Entry>, String> {
    sh(cli, container, root, LIST, &[dir], None).map(|out| parse_list(&out))
}

fn parse_list(out: &[u8]) -> Vec<Entry> {
    let mut entries: Vec<Entry> = out
        .split(|b| *b == 0)
        .filter_map(|record| {
            let record = String::from_utf8_lossy(record);
            let mut parts = record.splitn(6, '|');
            let folder = parts.next()? == "d";
            let link = parts.next()? == "l";
            let size = parts.next()?.parse().unwrap_or(0);
            let mode = parts.next()?.to_string();
            let owner = parts.next()?.to_string();
            let name = parts.next()?.to_string();
            (!name.is_empty()).then_some(Entry {
                name,
                folder,
                link,
                size,
                mode,
                owner,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.folder
            .cmp(&a.folder)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

/// `name` in the folder `dir` of a container.
pub fn join(dir: &str, name: &str) -> String {
    if name.starts_with('/') {
        normalize(name)
    } else {
        normalize(&format!("{dir}/{name}"))
    }
}

/// An absolute path without `.`, `..` nor doubled slashes.
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("/{}", parts.join("/"))
}

pub fn parent(path: &str) -> String {
    join(path, "..")
}

/// A name typed for a new file or a rename: one name, not a path.
pub fn file_name(name: &str) -> Result<&str, String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        Err(rust_i18n::t!("docker.bad_name", name = name).to_string())
    } else {
        Ok(name)
    }
}

pub fn make_folder(cli: &Cli, container: &str, root: bool, path: &str) -> Result<(), String> {
    sh(cli, container, root, r#"mkdir -- "$1""#, &[path], None).map(|_| ())
}

/// An empty file; one that is there already is left as it is.
pub fn make_file(cli: &Cli, container: &str, root: bool, path: &str) -> Result<(), String> {
    let script = r#"if [ -e "$1" ]; then echo "$1: file exists" >&2; exit 1; fi; : > "$1""#;
    sh(cli, container, root, script, &[path], None).map(|_| ())
}

pub fn rename(cli: &Cli, container: &str, root: bool, from: &str, to: &str) -> Result<(), String> {
    let script = r#"if [ -e "$2" ]; then echo "$2: file exists" >&2; exit 1; fi; mv -- "$1" "$2""#;
    sh(cli, container, root, script, &[from, to], None).map(|_| ())
}

pub fn delete(cli: &Cli, container: &str, root: bool, path: &str) -> Result<(), String> {
    if normalize(path) == "/" {
        return Err(rust_i18n::t!("docker.not_root_folder").to_string());
    }
    sh(cli, container, root, r#"rm -rf -- "$1""#, &[path], None).map(|_| ())
}

/// Writes over a file in place: it keeps its owner and its rights. A
/// container that doesn't run gets a copy.
pub fn write_back(
    cli: &Cli,
    container: &str,
    root: bool,
    local: &Path,
    path: &str,
) -> Result<(), String> {
    let bytes = std::fs::read(local).map_err(|e| e.to_string())?;
    let written = sh(cli, container, root, r#"cat > "$1""#, &[path], Some(bytes));
    match written {
        Err(e) if e.contains("is not running") => copy_in(cli, container, local, path),
        other => other.map(|_| ()),
    }
}

/// `docker cp` out of the container (a file or a folder), running or not.
/// A link is followed.
pub fn copy_out(cli: &Cli, container: &str, path: &str, local: &Path) -> Result<(), String> {
    let from = format!("{container}:{path}");
    let to = local.to_string_lossy().into_owned();
    cli.run_long(&["cp", "-L", "--", &from, &to]).map(|_| ())
}

pub fn copy_in(cli: &Cli, container: &str, local: &Path, path: &str) -> Result<(), String> {
    let from = local.to_string_lossy().into_owned();
    let to = format!("{container}:{path}");
    cli.run_long(&["cp", "--", &from, &to]).map(|_| ())
}

/// Where the copy of a file of a container goes to be edited: under
/// `folder`, its path in the container kept (two files of one name don't
/// meet), nothing in it going above `folder`.
pub fn local_copy(folder: &Path, container: &str, path: &str) -> PathBuf {
    let mut local = folder.join(container.replace(['/', '\\', ':', '.'], "_"));
    let normal = normalize(path);
    for part in Path::new(&normal).components() {
        if let Component::Normal(part) = part {
            local.push(part);
        }
    }
    local
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listing_is_read_folders_first_whatever_the_names_hold() {
        let out = b"f|-|12|-rw-r--r--|root|a|b\nc\0d|l|4|lrwxrwxrwx|app|bin\0f|-|0|?|?|.env\0";
        let entries = parse_list(out);
        assert_eq!(entries[0].name, "bin");
        assert!(entries[0].folder && entries[0].link);
        assert_eq!(entries[1].name, ".env");
        assert_eq!(entries[2].name, "a|b\nc");
        assert_eq!(entries[2].size, 12);
        assert_eq!(entries[2].owner, "root");
    }

    #[test]
    fn paths_stay_under_the_root() {
        assert_eq!(join("/a/b", "c"), "/a/b/c");
        assert_eq!(join("/a", "/etc//x/./y"), "/etc/x/y");
        assert_eq!(parent("/a"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(normalize("../../x"), "/x");
        assert!(file_name("a/b").is_err());
        assert!(file_name("..").is_err());
        assert!(file_name("-x").is_ok(), "given after -- or as a whole path");
        let local = local_copy(Path::new("/data"), "../x", "/../../etc/passwd");
        assert!(local.starts_with("/data/___x"), "{local:?}");
        assert!(local.ends_with("etc/passwd"));
    }

    #[test]
    fn a_helper_says_which_process_made_it() {
        assert_eq!(maker(&helper_name("a-1")), Some(std::process::id() as i32));
        assert_eq!(maker("ardoise-volume-12-x"), Some(12));
        assert_eq!(maker("ardoise-volume--1-x"), None);
        assert_eq!(maker("ardoise-volume-0-x"), None);
        assert_eq!(maker("web"), None);
        assert!(!machine().is_empty());
    }

    #[test]
    fn the_root_folder_is_never_deleted() {
        assert!(delete(&Cli::default(), "none", false, "/a/..").is_err());
    }
}
