//! What may come at the cursor of a Dockerfile: the instructions, their
//! options, the images and stages after `FROM`, the variables after `$`, the
//! files of the context after `COPY`. Made from the text only (and what the
//! caller knows of the images and of the folder), so it can be checked alone.

use std::path::Path;

use ardoise_plugin::protocol::{CompletionItem, Completions};
use rust_i18n::t;

/// What makes the editor ask while typing, besides a name.
pub const TRIGGERS: &[&str] = &[" ", "-", "=", ":", "/", "{"];

const INSTRUCTIONS: &[&str] = &[
    "FROM",
    "RUN",
    "CMD",
    "COPY",
    "ADD",
    "WORKDIR",
    "ENV",
    "ARG",
    "EXPOSE",
    "ENTRYPOINT",
    "USER",
    "VOLUME",
    "LABEL",
    "HEALTHCHECK",
    "SHELL",
    "STOPSIGNAL",
    "ONBUILD",
    "MAINTAINER",
];

/// Official images often started from.
const COMMON_IMAGES: &[&str] = &[
    "alpine",
    "busybox",
    "debian",
    "ubuntu",
    "nginx",
    "node",
    "python",
    "golang",
    "rust",
    "php",
    "ruby",
    "eclipse-temurin",
    "maven",
    "gradle",
    "postgres",
    "mariadb",
    "mysql",
    "redis",
    "httpd",
    "caddy",
    "scratch",
];

const PLATFORMS: &[&str] = &[
    "$BUILDPLATFORM",
    "$TARGETPLATFORM",
    "linux/amd64",
    "linux/arm64",
    "linux/arm/v7",
];

/// The variables a build has without declaring them as `ENV`.
const BUILD_ARGS: &[&str] = &[
    "BUILDPLATFORM",
    "TARGETPLATFORM",
    "TARGETOS",
    "TARGETARCH",
    "TARGETVARIANT",
    "BUILDOS",
    "BUILDARCH",
];

fn flags(instruction: &str) -> &'static [&'static str] {
    match instruction {
        "FROM" => &["--platform="],
        "RUN" => &["--mount=", "--network=", "--security="],
        "COPY" => &[
            "--from=",
            "--chown=",
            "--chmod=",
            "--link",
            "--exclude=",
            "--parents",
        ],
        "ADD" => &[
            "--chown=",
            "--chmod=",
            "--link",
            "--checksum=",
            "--keep-git-dir=",
            "--exclude=",
        ],
        "HEALTHCHECK" => &[
            "--interval=",
            "--timeout=",
            "--start-period=",
            "--start-interval=",
            "--retries=",
        ],
        _ => &[],
    }
}

fn item(label: impl Into<String>, detail: impl Into<String>) -> CompletionItem {
    CompletionItem {
        label: label.into(),
        insert: None,
        detail: detail.into(),
    }
}

fn inserting(label: &str, insert: String, detail: impl Into<String>) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
        insert: Some(insert),
        detail: detail.into(),
    }
}

/// The names the stages before the cursor were given.
fn stage_names(before: &[&str]) -> Vec<String> {
    before
        .iter()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            words.first().filter(|w| w.eq_ignore_ascii_case("from"))?;
            words
                .windows(2)
                .find(|w| w[0].eq_ignore_ascii_case("as"))
                .map(|w| w[1].to_string())
        })
        .collect()
}

/// The names `ARG` and `ENV` declared before the cursor.
fn variables(before: &[&str]) -> Vec<String> {
    let mut names = Vec::new();
    for line in before {
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else { continue };
        let env = first.eq_ignore_ascii_case("env");
        if !env && !first.eq_ignore_ascii_case("arg") {
            continue;
        }
        for (i, word) in words.enumerate() {
            // `ENV A=1 B=2`, or the old `ENV A the rest`.
            let name = match word.split_once('=') {
                Some((name, _)) => name,
                None if i == 0 => word,
                None => continue,
            };
            let good = !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit());
            if good && !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
            // The old form has one name only.
            if env && i == 0 && !word.contains('=') {
                break;
            }
        }
    }
    names
}

/// The files of `folder` (the build context) a partial path may go on with.
fn paths(folder: &Path, typed: &str) -> Vec<CompletionItem> {
    let directory = typed.rsplit_once('/').map_or("", |(dir, _)| dir);
    // Only what is in the context: a build sees nothing else.
    if directory.split('/').any(|part| part == "..") {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(folder.join(directory.trim_start_matches('/'))) else {
        return Vec::new();
    };
    let mut found: Vec<(bool, String)> = entries
        .filter_map(Result::ok)
        .take(2000)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let folder = e.file_type().ok()?.is_dir();
            (name != ".git").then_some((!folder, name))
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .map(|(file, name)| {
            if file {
                item(name, "")
            } else {
                item(format!("{name}/"), "")
            }
        })
        .collect()
}

/// What may come at `offset` (in characters). `images` are the references of
/// the images of the engine; `folder` is where the Dockerfile is.
pub fn complete(text: &str, offset: usize, images: &[String], folder: &Path) -> Completions {
    let cursor = text
        .char_indices()
        .nth(offset)
        .map_or(text.len(), |(at, _)| at);
    let head = &text[..cursor];
    let mut lines: Vec<&str> = head.split('\n').collect();
    let line = lines.pop().unwrap_or_default();
    if line.trim_start().starts_with('#') {
        return Completions::default();
    }
    // The word being typed, and where it starts (in characters).
    let word_at = line.rfind(char::is_whitespace).map_or(0, |i| {
        i + line[i..].chars().next().map_or(1, char::len_utf8)
    });
    let word = &line[word_at..];
    let from_of = |tail: &str| offset - tail.chars().count();
    let with_from = |items: Vec<CompletionItem>, from: usize| Completions {
        items,
        from: Some(from),
    };

    // A variable, wherever it is.
    if let Some(at) = word.rfind('$') {
        let tail = &word[at..];
        let braced = tail.starts_with("${");
        let name = tail.trim_start_matches(['$', '{']);
        if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            let mut declared = variables(&lines);
            declared.extend(BUILD_ARGS.iter().map(|s| s.to_string()));
            let items = declared
                .into_iter()
                .map(|n| {
                    let label = if braced {
                        format!("${{{n}}}")
                    } else {
                        format!("${n}")
                    };
                    item(label, "")
                })
                .collect();
            return with_from(items, from_of(tail));
        }
    }

    // The instruction of the line: that of the first line when it goes on
    // over several with `\`.
    let mut first = lines.len();
    while first > 0 && lines[first - 1].trim_end().ends_with('\\') {
        first -= 1;
    }
    let continued = first < lines.len();
    let start = if continued { lines[first] } else { line };
    let mut words: Vec<&str> = line[..word_at].split_whitespace().collect();
    if !continued && words.is_empty() {
        let items = INSTRUCTIONS
            .iter()
            .map(|name| {
                let key = format!("docker.in_{}", name.to_ascii_lowercase());
                inserting(name, format!("{name} "), t!(key.as_str()))
            })
            .collect();
        return with_from(items, from_of(word));
    }
    let mut instruction = start
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if !continued && !words.is_empty() {
        words.remove(0);
    }
    if instruction == "ONBUILD" {
        if words.is_empty() {
            let items = INSTRUCTIONS
                .iter()
                .filter(|n| !matches!(**n, "ONBUILD" | "FROM" | "MAINTAINER"))
                .map(|n| inserting(n, format!("{n} "), ""))
                .collect();
            return with_from(items, from_of(word));
        }
        instruction = words.remove(0).to_ascii_uppercase();
    }

    // An option and its value.
    if let Some((flag, value)) = word.split_once('=').filter(|(f, _)| f.starts_with("--")) {
        let items: Vec<CompletionItem> = match flag {
            "--from" => stage_names(&lines)
                .into_iter()
                .map(|s| item(s, t!("docker.stage")))
                .chain(images.iter().map(|i| item(i.clone(), t!("docker.image"))))
                .collect(),
            "--platform" => PLATFORMS.iter().map(|p| item(*p, "")).collect(),
            "--network" => ["default", "none", "host"].map(|v| item(v, "")).to_vec(),
            "--security" => ["sandbox", "insecure"].map(|v| item(v, "")).to_vec(),
            "--mount" if value.is_empty() => ["cache", "bind", "secret", "ssh", "tmpfs"]
                .map(|v| inserting(&format!("type={v}"), format!("type={v},"), ""))
                .to_vec(),
            _ => Vec::new(),
        };
        return with_from(items, from_of(value));
    }
    let positional = words.iter().filter(|w| !w.starts_with("--")).count();
    if word.starts_with('-') {
        // The options go before everything else.
        if positional > 0 {
            return Completions::default();
        }
        let items = flags(&instruction).iter().map(|f| item(*f, "")).collect();
        return with_from(items, from_of(word));
    }

    match instruction.as_str() {
        "FROM" => match positional {
            0 => {
                let mut items: Vec<CompletionItem> = stage_names(&lines)
                    .into_iter()
                    .map(|s| item(s, t!("docker.stage")))
                    .collect();
                items.extend(images.iter().map(|i| item(i.clone(), t!("docker.local"))));
                // With a tag being typed, only what is here is known.
                if !word.contains([':', '/']) {
                    items.extend(
                        COMMON_IMAGES
                            .iter()
                            .map(|i| item(*i, t!("docker.official"))),
                    );
                }
                with_from(items, from_of(word))
            }
            1 if !word.is_empty() => with_from(
                vec![inserting("AS", "AS ".into(), t!("docker.stage"))],
                from_of(word),
            ),
            _ => Completions::default(),
        },
        "HEALTHCHECK" if positional == 0 && !word.is_empty() => {
            with_from(["CMD", "NONE"].map(|v| item(v, "")).to_vec(), from_of(word))
        }
        "STOPSIGNAL" if positional == 0 => with_from(
            ["SIGTERM", "SIGINT", "SIGQUIT", "SIGKILL", "SIGHUP"]
                .map(|v| item(v, ""))
                .to_vec(),
            from_of(word),
        ),
        // The sources: files of the context, unless copied from elsewhere.
        // A list at each space would be in the way: a letter or a `/` first.
        "COPY" | "ADD" if !word.is_empty() && !words.iter().any(|w| w.starts_with("--from")) => {
            let name = word.rsplit('/').next().unwrap_or_default();
            with_from(paths(folder, word), from_of(name))
        }
        _ => Completions::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The labels at the `|` of a text, and what would be replaced.
    fn at(marked: &str) -> (Vec<String>, String) {
        at_in(marked, Path::new("/nowhere"))
    }

    fn at_in(marked: &str, folder: &Path) -> (Vec<String>, String) {
        let cursor = marked.find('|').unwrap();
        let text = marked.replace('|', "");
        let offset = marked[..cursor].chars().count();
        let images = ["nginx:1.27".to_string(), "acme/app:dev".to_string()];
        let found = complete(&text, offset, &images, folder);
        let replaced = found.from.map_or(String::new(), |from| {
            text.chars().skip(from).take(offset - from).collect()
        });
        (found.items.into_iter().map(|i| i.label).collect(), replaced)
    }

    fn has(labels: &[String], label: &str) -> bool {
        labels.iter().any(|l| l == label)
    }

    #[test]
    fn a_line_starts_with_an_instruction_unless_it_goes_on() {
        let (labels, replaced) = at("FROM a\n  ru|");
        assert!(has(&labels, "RUN") && has(&labels, "FROM"));
        assert_eq!(replaced, "ru");
        assert!(at("RUN echo \\\n  ec|").0.is_empty());
        assert!(at("# FR|").0.is_empty());
        let (labels, _) = at("ONBUILD |");
        assert!(has(&labels, "RUN") && !has(&labels, "FROM"));
    }

    #[test]
    fn after_from_come_the_images_and_the_stages_then_as() {
        let (labels, replaced) = at("FROM rust AS build\nFROM ngi|");
        assert_eq!(labels[0], "build");
        assert!(has(&labels, "nginx:1.27") && has(&labels, "alpine"));
        assert_eq!(replaced, "ngi");
        let (labels, replaced) = at("FROM nginx:|");
        assert!(has(&labels, "nginx:1.27") && !has(&labels, "alpine"));
        assert_eq!(replaced, "nginx:");
        assert_eq!(at("FROM nginx a|"), (vec!["AS".into()], "a".into()));
        assert!(at("FROM nginx |").0.is_empty(), "nothing at the space");
        assert!(at("FROM nginx AS x|").0.is_empty());
    }

    #[test]
    fn options_and_their_values() {
        let (labels, replaced) = at("COPY --|");
        assert!(has(&labels, "--from=") && has(&labels, "--chown="));
        assert_eq!(replaced, "--");
        let (labels, replaced) = at("FROM a AS build\nCOPY --from=bu|");
        assert_eq!(labels[0], "build");
        assert_eq!(replaced, "bu");
        let (labels, replaced) = at("RUN --mount=|");
        assert!(has(&labels, "type=cache"));
        assert_eq!(replaced, "");
        assert!(has(&at("FROM --platform=|").0, "linux/arm64"));
        assert!(at("RUN ls -|").0.is_empty(), "an option of the command");
        assert!(has(&at("RUN \\\n  --|").0, "--mount="));
    }

    #[test]
    fn variables_declared_before_come_after_the_dollar() {
        let text = "ARG VERSION=1\nENV A=1 B=2\nENV OLD the rest\nRUN echo $|\nARG LATER";
        let (labels, replaced) = at(text);
        for name in ["$VERSION", "$A", "$B", "$OLD", "$TARGETARCH"] {
            assert!(has(&labels, name), "{name} in {labels:?}");
        }
        assert!(!has(&labels, "$LATER") && !has(&labels, "$the"));
        assert_eq!(replaced, "$");
        let (labels, replaced) = at("ARG V\nFROM node:${V|");
        assert!(has(&labels, "${V}"));
        assert_eq!(replaced, "${V");
    }

    #[test]
    fn the_sources_of_a_copy_are_the_files_of_the_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let (labels, replaced) = at_in("COPY C|", dir.path());
        assert_eq!(labels, ["src/", "Cargo.toml"]);
        assert_eq!(replaced, "C");
        let (labels, replaced) = at_in("COPY src/|", dir.path());
        assert_eq!(labels, ["main.rs"]);
        assert_eq!(replaced, "");
        assert!(at_in("COPY ../|", dir.path()).0.is_empty());
        assert!(at_in("COPY --from=build sr|", dir.path()).0.is_empty());
        assert!(at_in("COPY |", dir.path()).0.is_empty());
    }

    #[test]
    fn offsets_are_in_characters() {
        let (labels, replaced) = at("# é à\nFRO|");
        assert!(has(&labels, "FROM"));
        assert_eq!(replaced, "FRO");
    }
}
