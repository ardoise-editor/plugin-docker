//! The files Docker runs: which they are, and the lines of what can be run
//! in them (the stages of a Dockerfile, the services of a compose file).

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dockerfile,
    Compose,
}

pub fn kind(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let yaml = name.ends_with(".yml") || name.ends_with(".yaml");
    let compose = ["compose", "docker-compose"].iter().any(|base| {
        name.strip_prefix(base)
            .is_some_and(|rest| rest.starts_with(['.', '-', '_']))
    });
    if yaml && compose {
        return Some(Kind::Compose);
    }
    let dockerfile = ["dockerfile", "containerfile"].iter().any(|base| {
        name == *base
            || name.ends_with(&format!(".{base}"))
            || name
                .strip_prefix(base)
                .is_some_and(|rest| rest.starts_with(['.', '-']))
    });
    // `Dockerfile.dockerignore` lists files: nothing to build.
    (dockerfile && !name.ends_with(".dockerignore")).then_some(Kind::Dockerfile)
}

/// A thing of a file that can be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// From 0.
    pub line: usize,
    pub what: What,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    /// The `services:` line: the whole file.
    All,
    Service(String),
    /// A `FROM`: its name (`AS name`), if it has one.
    Stage(Option<String>),
}

pub fn targets(kind: Kind, text: &str) -> Vec<Target> {
    match kind {
        Kind::Dockerfile => stages(text),
        Kind::Compose => services(text),
    }
}

fn stages(text: &str) -> Vec<Target> {
    text.lines()
        .enumerate()
        .filter_map(|(line, content)| {
            let mut words = content.split_whitespace();
            if !words.next()?.eq_ignore_ascii_case("from") {
                return None;
            }
            let rest: Vec<&str> = words.collect();
            let name = rest
                .windows(2)
                .find(|w| w[0].eq_ignore_ascii_case("as"))
                .map(|w| w[1].to_string());
            Some(Target {
                line,
                what: What::Stage(name),
            })
        })
        .collect()
}

/// The keys right under `services:`. Written as YAML blocks, as compose
/// files are; `docker compose` itself reads the file when it runs.
fn services(text: &str) -> Vec<Target> {
    let mut found = Vec::new();
    let mut inside = false;
    let mut depth = None;
    for (line, content) in text.lines().enumerate() {
        let body = content.trim_start();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let indent = content.len() - body.len();
        if indent == 0 {
            inside = body
                .strip_prefix("services:")
                .is_some_and(|rest| rest.trim().is_empty() || rest.trim().starts_with(['#', '&']));
            depth = None;
            if inside {
                found.push(Target {
                    line,
                    what: What::All,
                });
            }
            continue;
        }
        if !inside || indent != *depth.get_or_insert(indent) {
            continue;
        }
        let Some((key, rest)) = body.split_once(':') else {
            continue;
        };
        if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
            continue;
        }
        let name = key.trim().trim_matches(['"', '\'']);
        let valid = !name.is_empty()
            && !name.starts_with(['-', '.', '<'])
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if valid {
            found.push(Target {
                line,
                what: What::Service(name.to_string()),
            });
        }
    }
    found
}

/// The tag offered for the image of a Dockerfile: the name of its folder.
pub fn default_tag(path: &Path) -> String {
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let clean: String = folder
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim_matches('-');
    format!("{}:latest", if clean.is_empty() { "image" } else { clean })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_files_are_known_by_their_names() {
        for name in [
            "Dockerfile",
            "Dockerfile.dev",
            "api.dockerfile",
            "Containerfile",
            "dockerfile-prod",
        ] {
            assert_eq!(kind(Path::new(name)), Some(Kind::Dockerfile), "{name}");
        }
        for name in [
            "compose.yml",
            "compose.yaml",
            "docker-compose.yml",
            "docker-compose.prod.yaml",
            "compose-dev.yml",
        ] {
            assert_eq!(kind(Path::new(name)), Some(Kind::Compose), "{name}");
        }
        for name in [
            "main.rs",
            "composer.yml",
            "values.yaml",
            "Dockerfile.dockerignore",
            "dockerfiles.md",
            "compose.json",
        ] {
            assert_eq!(kind(Path::new(name)), None, "{name}");
        }
    }

    #[test]
    fn each_from_is_a_stage() {
        let text = "# FROM no\nFROM rust:1 AS build\nRUN x\n  from --platform=linux/amd64 alpine\n";
        assert_eq!(
            targets(Kind::Dockerfile, text),
            [
                Target {
                    line: 1,
                    what: What::Stage(Some("build".into()))
                },
                Target {
                    line: 3,
                    what: What::Stage(None)
                },
            ]
        );
    }

    #[test]
    fn the_services_are_the_keys_under_services() {
        let text = "name: x\nx-a: &a\n  b: 1\nservices: # all\n  web:\n    image: nginx\n    environment:\n      db: 1\n  # a: comment\n  \"db-1\": &db\n    <<: *a\nvolumes:\n  data:\n";
        let found: Vec<(usize, What)> = targets(Kind::Compose, text)
            .into_iter()
            .map(|t| (t.line, t.what))
            .collect();
        assert_eq!(
            found,
            [
                (3, What::All),
                (4, What::Service("web".into())),
                (9, What::Service("db-1".into())),
            ]
        );
    }

    #[test]
    fn the_tag_of_a_build_comes_from_its_folder() {
        assert_eq!(
            default_tag(Path::new("/a/My App/Dockerfile")),
            "my-app:latest"
        );
        assert_eq!(default_tag(Path::new("/__/Dockerfile")), "image:latest");
    }
}
