//! Registries: those `docker` is logged in to (read from its configuration),
//! and what one holds, asked to its HTTP API (v2) as `docker` would.
//!
//! A password never goes in the arguments of a command nor in a log. What is
//! kept to log in is only sent to the registry it is for, and to the token
//! server it names, over HTTPS (but for a registry of this machine).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine;
use rust_i18n::t;
use serde_json::Value;
use ureq::tls::{RootCerts, TlsConfig};

/// The name `docker` gives Docker Hub in its configuration.
#[cfg(test)]
pub const HUB_KEY: &str = "https://index.docker.io/v1/";
pub const HUB: &str = "docker.io";
/// The most pages asked of a long list.
const PAGES: usize = 20;

/// `host[:port]` from what the user typed or the configuration holds.
pub fn host(server: &str) -> String {
    let rest = server.trim();
    let rest = rest
        .strip_prefix("https://")
        .or_else(|| rest.strip_prefix("http://"))
        .unwrap_or(rest);
    let host = rest.split('/').next().unwrap_or_default().to_lowercase();
    match host.as_str() {
        "" | "index.docker.io" | "registry-1.docker.io" | "registry.hub.docker.com" => HUB.into(),
        _ => host,
    }
}

/// A host that can be one: what goes in a URL and in a command.
pub fn good_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with(['-', '.'])
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
}

/// A repository of a registry: lowercase words, `/` between them.
pub fn good_repository(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '/'])
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
        })
}

/// A registry of this machine, which may speak plain HTTP.
fn local(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    matches!(name, "localhost" | "127.0.0.1" | "::1")
}

/// How an image of a registry is named for `docker`.
pub fn reference(host: &str, repository: &str, tag: &str) -> String {
    if host == HUB {
        format!("{repository}:{tag}")
    } else {
        format!("{host}/{repository}:{tag}")
    }
}

/// `config.json` of `docker`.
pub fn config_path() -> Option<PathBuf> {
    match std::env::var_os("DOCKER_CONFIG").filter(|d| !d.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir).join("config.json")),
        None => std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".docker").join("config.json")),
    }
}

fn read_config() -> Value {
    config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn decoded(auth: &str) -> Option<(String, String)> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(auth.trim())
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (user, secret) = text.split_once(':')?;
    Some((user.to_string(), secret.to_string()))
}

/// The registries `docker` is logged in to: host → the key of the
/// configuration. No secret is read.
pub fn logged_in_of(config: &Value) -> BTreeMap<String, String> {
    ["auths", "credHelpers"]
        .iter()
        .filter_map(|section| config.get(section)?.as_object())
        .flat_map(|map| map.keys())
        // The tokens Docker Desktop keeps beside the registries.
        .filter(|key| !key.contains("-token") && !key.contains("token."))
        .map(|key| (host(key), key.clone()))
        .filter(|(host, _)| good_host(host))
        .collect()
}

pub fn logged_in() -> BTreeMap<String, String> {
    logged_in_of(&read_config())
}

/// Asks the credential helper `docker-credential-<name>` of `docker`.
fn from_helper(name: &str, server: &str) -> Option<(String, String)> {
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let mut child = Command::new(format!("docker-credential-{name}"))
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(server.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    let answer: Value = serde_json::from_slice(&out.stdout).ok()?;
    let user = answer["Username"].as_str()?;
    let secret = answer["Secret"].as_str()?;
    Some((user.to_string(), secret.to_string()))
}

/// The user and password `docker` kept for a registry.
fn credentials_of(config: &Value, wanted: &str) -> Option<(String, String)> {
    let key = logged_in_of(config).remove(wanted)?;
    let helper = config["credHelpers"][&key]
        .as_str()
        .or_else(|| config["credsStore"].as_str());
    let found = helper
        .and_then(|name| from_helper(name, &key))
        .or_else(|| config["auths"][&key]["auth"].as_str().and_then(decoded));
    // An identity token is not a password: asked without.
    found.filter(|(user, _)| user != "<token>")
}

/// What a `WWW-Authenticate` header asks for.
#[derive(Debug, PartialEq, Eq)]
enum Challenge {
    Basic,
    Bearer(BTreeMap<String, String>),
}

fn challenge(header: &str) -> Option<Challenge> {
    let (scheme, rest) = header.trim().split_once(' ').unwrap_or((header.trim(), ""));
    if scheme.eq_ignore_ascii_case("basic") {
        return Some(Challenge::Basic);
    }
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut params = BTreeMap::new();
    let mut rest = rest.trim();
    // `key="value"`, apart by commas; a value may hold commas itself.
    while let Some((key, after)) = rest.split_once('=') {
        let (value, next) = match after.strip_prefix('"') {
            Some(quoted) => quoted.split_once('"').unwrap_or((quoted, "")),
            None => after.split_once(',').unwrap_or((after, "")),
        };
        params.insert(key.trim().to_lowercase(), value.to_string());
        rest = next.trim_start_matches([',', ' ']);
    }
    Some(Challenge::Bearer(params))
}

/// The path of the next page, from a `Link` header.
fn next_page(link: &str) -> Option<String> {
    let (url, rel) = link.split_once('>')?;
    if !rel.contains("rel=\"next\"") && !rel.contains("rel=next") {
        return None;
    }
    let url = url.trim().strip_prefix('<')?;
    // Whatever host it names, only its path is followed, on the same one.
    match url.split_once("://") {
        Some((_, rest)) => rest.find('/').map(|at| rest[at..].to_string()),
        None => url.starts_with('/').then(|| url.to_string()),
    }
}

struct Answer {
    status: u16,
    asks: String,
    link: String,
    body: String,
}

/// A registry to ask.
pub struct Registry {
    agent: ureq::Agent,
    base: String,
    credentials: Option<(String, String)>,
    /// Plain HTTP is accepted from the token server too (a registry of this
    /// machine).
    plain: bool,
}

fn basic(user: &str, password: &str) -> String {
    let both = format!("{user}:{password}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(both)
    )
}

impl Registry {
    pub fn new(host: &str, credentials: Option<(String, String)>) -> Result<Registry, String> {
        if !good_host(host) {
            return Err(t!("docker.bad_name", name = host).to_string());
        }
        let plain = local(host);
        let api = if host == HUB {
            "registry-1.docker.io"
        } else {
            host
        };
        let scheme = if plain { "http" } else { "https" };
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            // The certificates of the system: a private registry of a
            // company is trusted as `docker` trusts it.
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        Ok(Registry {
            agent: ureq::Agent::new_with_config(config),
            base: format!("{scheme}://{api}"),
            credentials,
            plain,
        })
    }

    /// With what `docker` kept for it.
    pub fn of_docker(host: &str) -> Result<Registry, String> {
        Registry::new(host, credentials_of(&read_config(), host))
    }

    fn call(
        &self,
        url: &str,
        authorization: Option<&str>,
        query: &[(&str, &str)],
    ) -> Result<Answer, String> {
        let mut request = self.agent.get(url).header("Accept", "application/json");
        if let Some(value) = authorization {
            request = request.header("Authorization", value);
        }
        for (key, value) in query {
            request = request.query(*key, *value);
        }
        let mut response = request.call().map_err(|e| e.to_string())?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string()
        };
        let (asks, link) = (header("www-authenticate"), header("link"));
        Ok(Answer {
            status: response.status().as_u16(),
            asks,
            link,
            body: response
                .body_mut()
                .read_to_string()
                .map_err(|e| e.to_string())?,
        })
    }

    /// The `Authorization` a challenge asks for.
    fn authorize(&self, asks: &str) -> Result<String, String> {
        let credentials = self.credentials.as_ref();
        match challenge(asks) {
            Some(Challenge::Basic) => credentials
                .map(|(u, p)| basic(u, p))
                .ok_or_else(|| t!("docker.registry_login_needed").to_string()),
            Some(Challenge::Bearer(params)) => {
                let realm = params.get("realm").map_or("", String::as_str);
                if !(realm.starts_with("https://") || self.plain && realm.starts_with("http://")) {
                    return Err(t!("docker.registry_unsafe", url = realm).to_string());
                }
                let query: Vec<(&str, &str)> = ["service", "scope"]
                    .iter()
                    .filter_map(|k| Some((*k, params.get(*k)?.as_str())))
                    .collect();
                let own = credentials.map(|(u, p)| basic(u, p));
                let answer = self.call(realm, own.as_deref(), &query)?;
                if answer.status != 200 {
                    return Err(t!("docker.registry_login_needed").to_string());
                }
                let body: Value = serde_json::from_str(&answer.body).map_err(|e| e.to_string())?;
                let token = body["token"]
                    .as_str()
                    .or_else(|| body["access_token"].as_str())
                    .ok_or_else(|| t!("docker.registry_login_needed").to_string())?;
                Ok(format!("Bearer {token}"))
            }
            None => Err(t!("docker.registry_login_needed").to_string()),
        }
    }

    /// All the pages of a list of names.
    fn names(&self, path: &str, field: &str) -> Result<Vec<String>, String> {
        let mut names = Vec::new();
        let mut path = path.to_string();
        for _ in 0..PAGES {
            let url = format!("{}{path}", self.base);
            let mut answer = self.call(&url, None, &[])?;
            if answer.status == 401 {
                let authorization = self.authorize(&answer.asks)?;
                answer = self.call(&url, Some(&authorization), &[])?;
            }
            match answer.status {
                200 => {}
                401 | 403 => return Err(t!("docker.registry_denied").to_string()),
                404 => return Err(t!("docker.registry_not_found").to_string()),
                code => return Err(format!("HTTP {code}")),
            }
            let body: Value = serde_json::from_str(&answer.body).map_err(|e| e.to_string())?;
            names.extend(
                body[field]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string),
            );
            match next_page(&answer.link) {
                Some(next) => path = next,
                None => break,
            }
        }
        Ok(names)
    }

    /// The repositories, where the registry lists them (Docker Hub and the
    /// big forges don't).
    pub fn repositories(&self) -> Result<Vec<String>, String> {
        self.names("/v2/_catalog?n=200", "repositories")
    }

    pub fn tags(&self, repository: &str) -> Result<Vec<String>, String> {
        if !good_repository(repository) {
            return Err(t!("docker.bad_name", name = repository).to_string());
        }
        // The official images of Docker Hub live under `library/`.
        let full = if self.base.ends_with("registry-1.docker.io") && !repository.contains('/') {
            format!("library/{repository}")
        } else {
            repository.to_string()
        };
        let mut tags = self.names(&format!("/v2/{full}/tags/list?n=200"), "tags")?;
        tags.sort_by(|a, b| versions(b, a));
        // `latest` first, then the highest.
        if let Some(at) = tags.iter().position(|t| t == "latest") {
            let latest = tags.remove(at);
            tags.insert(0, latest);
        }
        Ok(tags)
    }
}

/// A tag cut into runs of digits and runs of the rest.
fn runs(tag: &str) -> impl Iterator<Item = (bool, &str)> {
    let mut rest = tag;
    std::iter::from_fn(move || {
        let digits = rest.chars().next()?.is_ascii_digit();
        let end = rest
            .find(|c: char| c.is_ascii_digit() != digits)
            .unwrap_or(rest.len());
        let (run, after) = rest.split_at(end);
        rest = after;
        Some((digits, run))
    })
}

/// Tags as versions: the numbers by their value (`3.21` after `3.9`).
fn versions(a: &str, b: &str) -> Ordering {
    let (mut left, mut right) = (runs(a), runs(b));
    loop {
        let order = match (left.next(), right.next()) {
            (Some((true, x)), Some((true, y))) => {
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                x.len().cmp(&y.len()).then_with(|| x.cmp(y))
            }
            // A number comes after a word: versions before names.
            (Some((x_digits, x)), Some((y_digits, y))) => {
                x_digits.cmp(&y_digits).then_with(|| x.cmp(y))
            }
            (x, y) => return x.is_some().cmp(&y.is_some()).then_with(|| a.cmp(b)),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::mpsc::{channel, Receiver};

    #[test]
    fn tags_are_ordered_as_versions() {
        let mut tags = vec!["3.9.6", "edge", "3.21", "3.10", "3.9", "20260101", "3.21.1"];
        tags.sort_by(|a, b| versions(b, a));
        assert_eq!(
            tags,
            ["20260101", "3.21.1", "3.21", "3.10", "3.9.6", "3.9", "edge"]
        );
        assert_eq!(versions("a", "a"), Ordering::Equal);
    }

    #[test]
    fn hosts_and_repositories() {
        assert_eq!(host(HUB_KEY), HUB);
        assert_eq!(host(""), HUB);
        assert_eq!(
            host("https://Registry.Example.com:5000/v2/"),
            "registry.example.com:5000"
        );
        assert!(good_host("localhost:5000") && !good_host("-x") && !good_host("a b"));
        assert!(!good_host("user@evil"));
        assert!(good_repository("acme/app-1.x") && good_repository("nginx"));
        for bad in ["", "-a", "a//b", "../a", "A", "a?x=1", "a#"] {
            assert!(!good_repository(bad), "{bad}");
        }
        assert_eq!(reference(HUB, "nginx", "1"), "nginx:1");
        assert_eq!(reference("ghcr.io", "a/b", "1"), "ghcr.io/a/b:1");
        assert!(local("127.0.0.1:5000") && local("[::1]:5000"));
        assert!(!local("localhost.evil.com") && !local("127.0.0.1.evil.com"));
    }

    #[test]
    fn the_configuration_says_where_docker_is_logged_in() {
        let config = serde_json::json!({
            "auths": {
                HUB_KEY: {"auth": base64::engine::general_purpose::STANDARD.encode("me:pa:ss")},
                "https://index.docker.io/v1/access-token": {},
                "registry.gitlab.com": {},
            },
            "credHelpers": {"123.dkr.ecr.eu-west-1.amazonaws.com": "ecr-login"},
        });
        let hosts: Vec<String> = logged_in_of(&config).into_keys().collect();
        assert_eq!(
            hosts,
            [
                "123.dkr.ecr.eu-west-1.amazonaws.com",
                HUB,
                "registry.gitlab.com"
            ]
        );
        assert_eq!(
            credentials_of(&config, HUB),
            Some(("me".into(), "pa:ss".into()))
        );
        assert_eq!(credentials_of(&config, "registry.gitlab.com"), None);
        assert_eq!(credentials_of(&config, "other.example.com"), None);
    }

    #[test]
    fn challenges_and_pages() {
        let Some(Challenge::Bearer(params)) = challenge(
            r#"Bearer realm="https://a/token",service="r",scope="repository:x:pull,push""#,
        ) else {
            panic!("a bearer");
        };
        assert_eq!(params["realm"], "https://a/token");
        assert_eq!(params["scope"], "repository:x:pull,push");
        assert_eq!(challenge("Basic realm=\"r\""), Some(Challenge::Basic));
        assert_eq!(challenge("Digest x"), None);
        assert_eq!(
            next_page("</v2/_catalog?last=b&n=2>; rel=\"next\""),
            Some("/v2/_catalog?last=b&n=2".into())
        );
        assert_eq!(
            next_page("<https://elsewhere/v2/x?n=2>; rel=\"next\""),
            Some("/v2/x?n=2".into())
        );
        assert_eq!(next_page("</a>; rel=\"prev\""), None);
        assert_eq!(next_page(""), None);
    }

    /// A registry that gives these answers in turn, and tells what it was
    /// asked: (first line, `Authorization`).
    fn fake(answers: Vec<String>) -> (String, Receiver<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (tx, rx) = channel();
        let here = address.clone();
        std::thread::spawn(move || {
            for answer in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut first, mut authorization) = (String::new(), String::new());
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("authorization") {
                            authorization = value.trim().to_string();
                        }
                    }
                }
                tx.send((first.trim().to_string(), authorization)).unwrap();
                stream
                    .write_all(answer.replace("HERE", &here).as_bytes())
                    .unwrap();
            }
        });
        (address, rx)
    }

    fn reply(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn a_token_is_asked_with_the_password_then_used_page_after_page() {
        let ask = reply(
            "401 Unauthorized",
            "WWW-Authenticate: Bearer realm=\"http://HERE/token\",service=\"test\"\r\n",
            "",
        );
        let token = reply("200 OK", "", r#"{"token":"abc"}"#);
        let (address, asked) = fake(vec![
            ask.clone(),
            token.clone(),
            reply(
                "200 OK",
                "Link: </v2/app/tags/list?last=1.0&n=200>; rel=\"next\"\r\n",
                r#"{"tags":["1.0"]}"#,
            ),
            ask,
            token,
            reply("200 OK", "", r#"{"tags":["latest","2.0"]}"#),
        ]);
        let registry = Registry::new(&address, Some(("me".into(), "secret".into()))).unwrap();
        assert_eq!(registry.tags("app").unwrap(), ["latest", "2.0", "1.0"]);
        let asked: Vec<(String, String)> = asked.iter().take(6).collect();
        assert_eq!(asked[0].1, "", "nothing sent before it is asked");
        assert!(asked[1].0.starts_with("GET /token?service=test"));
        assert_eq!(asked[1].1, basic("me", "secret"));
        assert_eq!(asked[2].1, "Bearer abc");
        assert!(asked[3].0.contains("last=1.0"));
    }

    #[test]
    fn a_refusal_says_to_log_in() {
        let (address, _asked) = fake(vec![reply(
            "401 Unauthorized",
            "WWW-Authenticate: Basic realm=\"r\"\r\n",
            "",
        )]);
        let registry = Registry::new(&address, None).unwrap();
        assert!(registry.repositories().is_err());
        assert!(Registry::new("bad host", None).is_err());
    }

    #[test]
    fn the_token_server_of_a_far_registry_must_be_https() {
        let registry = Registry::new("registry.example.com", None).unwrap();
        let refused = registry.authorize(r#"Bearer realm="http://evil.example.com/t""#);
        assert!(refused.is_err());
    }
}
