//! The Docker engine, through the `docker` command: what the user's shell
//! would see (contexts, `DOCKER_HOST`, the Compose plugin).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Deserialize;

/// How long a short command may take (a daemon that hangs must not hang the
/// plugin).
const SHORT: Duration = Duration::from_secs(30);
/// Removals, stops and cleanups of a big engine.
pub const LONG: Duration = Duration::from_secs(900);

/// How to call `docker`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cli {
    /// The context chosen in the panel (none: the one of the CLI). It is
    /// given to each command: the user's own `docker` stays as it was.
    pub context: Option<String>,
}

impl Cli {
    /// The arguments with the context before them.
    pub fn args<S: AsRef<str>>(&self, args: &[S]) -> Vec<String> {
        let mut all = Vec::new();
        if let Some(context) = &self.context {
            all.push("--context".to_string());
            all.push(context.clone());
        }
        all.extend(args.iter().map(|a| a.as_ref().to_string()));
        all
    }

    pub fn command<S: AsRef<str>>(&self, args: &[S]) -> Command {
        let mut command = Command::new("docker");
        command.args(self.args(args));
        command
    }

    /// What a command prints; its error output if it fails.
    pub fn run<S: AsRef<str>>(&self, args: &[S]) -> Result<String, String> {
        self.run_with(args, None, SHORT)
            .map(|out| String::from_utf8_lossy(&out).into_owned())
    }

    /// The same for a command that may be long.
    pub fn run_long<S: AsRef<str>>(&self, args: &[S]) -> Result<String, String> {
        self.run_with(args, None, LONG)
            .map(|out| String::from_utf8_lossy(&out).into_owned())
    }

    /// Runs with `input` on the standard input.
    pub fn run_with<S: AsRef<str>>(
        &self,
        args: &[S],
        input: Option<Vec<u8>>,
        limit: Duration,
    ) -> Result<Vec<u8>, String> {
        let mut child = self
            .command(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("docker: {e}"))?;
        if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
            std::thread::spawn(move || {
                let _ = std::io::Write::write_all(&mut stdin, &bytes);
            });
        }
        let read = |pipe: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut bytes);
                }
                bytes
            })
        };
        let out = read(child.stdout.take().map(|p| Box::new(p) as _));
        let err = read(child.stderr.take().map(|p| Box::new(p) as _));
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if start.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                return Err(rust_i18n::t!("docker.timeout").to_string());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let out = out.join().unwrap_or_default();
        let err = err.join().unwrap_or_default();
        if status.success() {
            Ok(out)
        } else {
            let said = String::from_utf8_lossy(&err).trim().to_string();
            Err(if said.is_empty() {
                format!("docker: {status}")
            } else {
                said
            })
        }
    }

    /// One JSON object a line.
    fn lines<T: DeserializeOwned>(&self, args: &[&str]) -> Result<Vec<T>, String> {
        parse_lines(&self.run(args)?)
    }

    pub fn containers(&self) -> Result<Vec<Container>, String> {
        let all: Vec<Container> =
            self.lines(&["ps", "-a", "--no-trunc", "--format", CONTAINER_FORMAT])?;
        Ok(all.into_iter().filter(|c| c.helper.is_empty()).collect())
    }

    pub fn images(&self) -> Result<Vec<Image>, String> {
        self.lines(&["images", "--format", "{{json .}}"])
    }

    pub fn volumes(&self) -> Result<Vec<Volume>, String> {
        self.lines(&["volume", "ls", "--format", "{{json .}}"])
    }

    pub fn unused_volumes(&self) -> Result<Vec<String>, String> {
        Ok(self
            .run(&["volume", "ls", "-q", "-f", "dangling=true"])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    pub fn networks(&self) -> Result<Vec<Network>, String> {
        self.lines(&["network", "ls", "--format", "{{json .}}"])
    }

    pub fn contexts(&self) -> Result<Vec<DockerContext>, String> {
        self.lines(&["context", "ls", "--format", "{{json .}}"])
    }

    pub fn disk_usage(&self) -> Result<Vec<Usage>, String> {
        parse_lines(&self.run_long(&["system", "df", "--format", "{{json .}}"])?)
    }

    /// The ids (whole) of the images that containers were made from.
    pub fn used_image_ids(&self) -> Result<Vec<String>, String> {
        let ids = self.run(&["ps", "-aq", "--no-trunc"])?;
        let ids: Vec<&str> = ids.lines().collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["inspect", "--format", "{{.Image}}"];
        args.extend(ids);
        Ok(self.run(&args)?.lines().map(str::to_string).collect())
    }
}

pub fn parse_lines<T: DeserializeOwned>(out: &str) -> Result<Vec<T>, String> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| e.to_string()))
        .collect()
}

/// Labels are asked one by one: the list `{{json .}}` gives of them can't be
/// cut apart (a value may hold commas).
const CONTAINER_FORMAT: &str = concat!(
    r#"{"id":{{json .ID}},"name":{{json .Names}},"image":{{json .Image}},"#,
    r#""state":{{json .State}},"status":{{json .Status}},"ports":{{json .Ports}},"#,
    r#""project":{{json (.Label "com.docker.compose.project")}},"#,
    r#""service":{{json (.Label "com.docker.compose.service")}},"#,
    r#""files":{{json (.Label "com.docker.compose.project.config_files")}},"#,
    r#""dir":{{json (.Label "com.docker.compose.project.working_dir")}},"#,
    // `files::HELPER_LABEL`.
    r#""helper":{{json (.Label "dev.ardoise.helper")}}}"#,
);

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub ports: String,
    pub project: String,
    pub service: String,
    pub files: String,
    pub dir: String,
    /// Set on the containers the plugin makes for itself.
    pub helper: String,
}

impl Container {
    pub fn running(&self) -> bool {
        self.state == "running"
    }

    pub fn paused(&self) -> bool {
        self.state == "paused"
    }

    /// What `container prune` removes.
    pub fn prunable(&self) -> bool {
        matches!(self.state.as_str(), "exited" | "created" | "dead")
    }
}

/// An id cut to the 12 characters `docker` shows.
pub fn short(id: &str) -> &str {
    let id = id.strip_prefix("sha256:").unwrap_or(id);
    id.get(..12).unwrap_or(id)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Image {
    #[serde(rename = "ID")]
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub size: String,
    pub created_since: String,
}

impl Image {
    pub fn dangling(&self) -> bool {
        self.repository == "<none>"
    }

    /// How `docker` is told which one: its name, or its id without one.
    pub fn reference(&self) -> String {
        if self.dangling() || self.tag == "<none>" {
            self.id.clone()
        } else {
            format!("{}:{}", self.repository, self.tag)
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Volume {
    pub name: String,
    pub driver: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Network {
    #[serde(rename = "ID")]
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
}

impl Network {
    /// One of the three the engine makes and keeps.
    pub fn builtin(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct DockerContext {
    pub name: String,
    pub current: bool,
    pub docker_endpoint: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Usage {
    #[serde(rename = "Type")]
    pub kind: String,
    pub total_count: String,
    pub active: String,
    pub size: String,
    pub reclaimable: String,
}

/// A value typed by the user where `docker` waits for a name: it must not be
/// taken for an option.
pub fn name_arg(value: &str) -> Result<&str, String> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') {
        Err(rust_i18n::t!("docker.bad_name", name = value).to_string())
    } else {
        Ok(value)
    }
}

/// Words as a shell cuts them: spaces apart, quotes keep them together.
pub fn split_words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' && matches!(chars.peek(), Some('"' | '\\')) => {
                word.extend(chars.next());
            }
            Some(_) => word.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                started = true;
            }
            None if c == '\\' => {
                word.extend(chars.next());
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(rust_i18n::t!("docker.open_quote").to_string());
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

/// A word as `sh` reads it back.
pub fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_context_goes_before_each_command() {
        let mut cli = Cli::default();
        assert_eq!(cli.args(&["ps"]), ["ps"]);
        cli.context = Some("remote".into());
        assert_eq!(cli.args(&["ps", "-a"]), ["--context", "remote", "ps", "-a"]);
    }

    #[test]
    fn words_are_cut_as_a_shell_cuts_them() {
        assert_eq!(
            split_words(r#"-p 80:80 -e "A=b c" -v '/a b:/c' x\ y """#).unwrap(),
            ["-p", "80:80", "-e", "A=b c", "-v", "/a b:/c", "x y", ""]
        );
        assert!(split_words("\"open").is_err());
        assert!(split_words("  ").unwrap().is_empty());
        assert_eq!(quote("a'b"), r"'a'\''b'");
    }

    #[test]
    fn a_name_is_not_an_option() {
        assert_eq!(name_arg(" nginx "), Ok("nginx"));
        assert!(name_arg("--privileged").is_err());
        assert!(name_arg("").is_err());
    }

    #[test]
    fn lines_of_docker_are_read() {
        let images: Vec<Image> = parse_lines(
            "{\"ID\":\"abc\",\"Repository\":\"<none>\",\"Tag\":\"<none>\",\"Size\":\"1MB\"}\n\
             {\"ID\":\"def\",\"Repository\":\"nginx\",\"Tag\":\"1\",\"Extra\":1}\n",
        )
        .unwrap();
        assert_eq!(images[0].reference(), "abc");
        assert_eq!(images[1].reference(), "nginx:1");
        assert_eq!(short("sha256:0123456789abcdef"), "0123456789ab");
    }
}
