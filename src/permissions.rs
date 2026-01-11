//! Deny-by-default capability model.
//!
//! Every op that touches the outside world calls one of the `check_*`
//! methods *in Rust* before doing any work. JS code cannot bypass these
//! checks: the only way to reach the file system, network, environment or
//! subprocesses is through ops, and ops consult this struct.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// A grant for one permission category.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Grant<T> {
    /// Nothing allowed (the default).
    #[default]
    Denied,
    /// Everything in this category allowed.
    All,
    /// Only the listed items allowed.
    Only(Vec<T>),
}

/// A network allow-list entry: `host` or `host:port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetEntry {
    pub host: String,
    pub port: Option<u16>,
}

impl NetEntry {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty host in --allow-net".into());
        }
        // `[::1]:8080` style IPv6
        if let Some(rest) = s.strip_prefix('[') {
            let end = rest
                .find(']')
                .ok_or_else(|| format!("invalid host '{s}'"))?;
            let host = rest[..end].to_string();
            let after = &rest[end + 1..];
            let port = match after.strip_prefix(':') {
                Some(p) => Some(p.parse().map_err(|_| format!("invalid port in '{s}'"))?),
                None if after.is_empty() => None,
                None => return Err(format!("invalid host '{s}'")),
            };
            return Ok(NetEntry { host, port });
        }
        match s.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => {
                let port = p.parse().map_err(|_| format!("invalid port in '{s}'"))?;
                Ok(NetEntry {
                    host: h.to_ascii_lowercase(),
                    port: Some(port),
                })
            }
            _ => Ok(NetEntry {
                host: s.to_ascii_lowercase(),
                port: None,
            }),
        }
    }

    fn matches(&self, host: &str, port: Option<u16>) -> bool {
        self.host.eq_ignore_ascii_case(host)
            && match self.port {
                None => true,
                Some(p) => port == Some(p),
            }
    }
}

/// The permission set of one runtime instance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Permissions {
    pub read: Grant<PathBuf>,
    pub write: Grant<PathBuf>,
    pub net: Grant<NetEntry>,
    pub env: Grant<String>,
    pub run: Grant<String>,
}

/// Error raised when a capability is missing. Surfaces in JS as
/// `harbor.errors.PermissionDenied`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDenied {
    pub kind: &'static str,
    pub detail: String,
}

impl fmt::Display for PermissionDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Requires {} access to {}, run again with the --allow-{} flag",
            self.kind, self.detail, self.kind
        )
    }
}

impl std::error::Error for PermissionDenied {}

/// Lexically normalise a path against `cwd` (no symlink resolution, no I/O).
/// `..` never escapes past the root.
pub fn normalize_path(path: &Path, cwd: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

fn path_allowed(grant: &Grant<PathBuf>, path: &Path) -> bool {
    match grant {
        Grant::Denied => false,
        Grant::All => true,
        // `starts_with` on Path is component-wise: /tmp/a does not match /tmp/ab.
        Grant::Only(list) => list.iter().any(|allowed| path.starts_with(allowed)),
    }
}

impl Permissions {
    /// Everything allowed (`-A`).
    pub fn allow_all() -> Self {
        Permissions {
            read: Grant::All,
            write: Grant::All,
            net: Grant::All,
            env: Grant::All,
            run: Grant::All,
        }
    }

    /// Try to consume one CLI flag. Returns `Ok(true)` if it was a
    /// permission flag, `Ok(false)` if it is not ours.
    pub fn parse_flag(&mut self, flag: &str, cwd: &Path) -> Result<bool, String> {
        if flag == "-A" || flag == "--allow-all" {
            *self = Self::allow_all();
            return Ok(true);
        }
        let Some(rest) = flag.strip_prefix("--allow-") else {
            return Ok(false);
        };
        let (name, value) = match rest.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (rest, None),
        };
        let items: Option<Vec<&str>> = value.map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect()
        });
        match name {
            "read" | "write" => {
                let grant = match items {
                    None => Grant::All,
                    Some(list) => Grant::Only(
                        list.iter()
                            .map(|p| normalize_path(Path::new(p), cwd))
                            .collect(),
                    ),
                };
                let slot = if name == "read" {
                    &mut self.read
                } else {
                    &mut self.write
                };
                *slot = merge(std::mem::take(slot), grant);
            }
            "net" => {
                let grant = match items {
                    None => Grant::All,
                    Some(list) => Grant::Only(
                        list.iter()
                            .map(|s| NetEntry::parse(s))
                            .collect::<Result<_, _>>()?,
                    ),
                };
                self.net = merge(std::mem::take(&mut self.net), grant);
            }
            "env" | "run" => {
                let grant = match items {
                    None => Grant::All,
                    Some(list) => Grant::Only(list.iter().map(|s| s.to_string()).collect()),
                };
                let slot = if name == "env" {
                    &mut self.env
                } else {
                    &mut self.run
                };
                *slot = merge(std::mem::take(slot), grant);
            }
            _ => return Err(format!("unknown permission flag '{flag}'")),
        }
        Ok(true)
    }

    pub fn check_read(&self, path: &Path, cwd: &Path) -> Result<PathBuf, PermissionDenied> {
        let p = normalize_path(path, cwd);
        if path_allowed(&self.read, &p) {
            Ok(p)
        } else {
            Err(PermissionDenied {
                kind: "read",
                detail: format!("\"{}\"", p.display()),
            })
        }
    }

    pub fn check_write(&self, path: &Path, cwd: &Path) -> Result<PathBuf, PermissionDenied> {
        let p = normalize_path(path, cwd);
        if path_allowed(&self.write, &p) {
            Ok(p)
        } else {
            Err(PermissionDenied {
                kind: "write",
                detail: format!("\"{}\"", p.display()),
            })
        }
    }

    pub fn check_net(&self, host: &str, port: Option<u16>) -> Result<(), PermissionDenied> {
        let ok = match &self.net {
            Grant::Denied => false,
            Grant::All => true,
            Grant::Only(list) => list.iter().any(|e| e.matches(host, port)),
        };
        if ok {
            Ok(())
        } else {
            let detail = match port {
                Some(p) => format!("\"{host}:{p}\""),
                None => format!("\"{host}\""),
            };
            Err(PermissionDenied {
                kind: "net",
                detail,
            })
        }
    }

    pub fn check_env(&self, key: &str) -> Result<(), PermissionDenied> {
        let ok = match &self.env {
            Grant::Denied => false,
            Grant::All => true,
            Grant::Only(list) => list.iter().any(|k| k == key),
        };
        if ok {
            Ok(())
        } else {
            Err(PermissionDenied {
                kind: "env",
                detail: format!("\"{key}\""),
            })
        }
    }

    /// Checks access to the whole environment (e.g. `harbor.env.toObject()`).
    pub fn check_env_all(&self) -> Result<(), PermissionDenied> {
        if self.env == Grant::All {
            Ok(())
        } else {
            Err(PermissionDenied {
                kind: "env",
                detail: "all environment variables".into(),
            })
        }
    }

    pub fn check_run(&self, cmd: &str) -> Result<(), PermissionDenied> {
        let base = Path::new(cmd)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ok = match &self.run {
            Grant::Denied => false,
            Grant::All => true,
            Grant::Only(list) => list.iter().any(|c| c == cmd || *c == base),
        };
        if ok {
            Ok(())
        } else {
            Err(PermissionDenied {
                kind: "run",
                detail: format!("\"{cmd}\""),
            })
        }
    }
}

/// Repeated flags accumulate; `All` absorbs everything.
fn merge<T>(a: Grant<T>, b: Grant<T>) -> Grant<T> {
    match (a, b) {
        (Grant::All, _) | (_, Grant::All) => Grant::All,
        (Grant::Denied, x) | (x, Grant::Denied) => x,
        (Grant::Only(mut x), Grant::Only(y)) => {
            x.extend(y);
            Grant::Only(x)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd() -> PathBuf {
        PathBuf::from("/work")
    }

    fn parse(flags: &[&str]) -> Permissions {
        let mut p = Permissions::default();
        for f in flags {
            assert!(p.parse_flag(f, &cwd()).unwrap(), "{f} not recognised");
        }
        p
    }

    #[test]
    fn default_denies_everything() {
        let p = Permissions::default();
        assert!(p.check_read(Path::new("/etc/passwd"), &cwd()).is_err());
        assert!(p.check_write(Path::new("x"), &cwd()).is_err());
        assert!(p.check_net("example.com", Some(80)).is_err());
        assert!(p.check_env("HOME").is_err());
        assert!(p.check_run("ls").is_err());
    }

    #[test]
    fn allow_all_flag() {
        let p = parse(&["-A"]);
        assert_eq!(p, Permissions::allow_all());
        assert!(p.check_run("/bin/ls").is_ok());
    }

    #[test]
    fn read_list_is_component_prefix() {
        let p = parse(&["--allow-read=/tmp/a,data"]);
        assert!(p.check_read(Path::new("/tmp/a"), &cwd()).is_ok());
        assert!(p.check_read(Path::new("/tmp/a/b/c.txt"), &cwd()).is_ok());
        assert!(p.check_read(Path::new("/tmp/ab"), &cwd()).is_err());
        assert!(p.check_read(Path::new("/tmp"), &cwd()).is_err());
        // relative entries resolve against cwd
        assert!(p.check_read(Path::new("/work/data/x"), &cwd()).is_ok());
        assert!(p.check_read(Path::new("data/x"), &cwd()).is_ok());
        // write is still denied
        assert!(p.check_write(Path::new("/tmp/a/x"), &cwd()).is_err());
    }

    #[test]
    fn dotdot_cannot_escape_allowed_dir() {
        let p = parse(&["--allow-read=/srv/public"]);
        assert!(
            p.check_read(Path::new("/srv/public/../secret"), &cwd())
                .is_err()
        );
        assert!(
            p.check_read(Path::new("/srv/public/./x/../y"), &cwd())
                .is_ok()
        );
        assert_eq!(
            normalize_path(Path::new("../../../../etc"), Path::new("/a/b")),
            PathBuf::from("/etc")
        );
    }

    #[test]
    fn net_host_and_port_matching() {
        let p = parse(&["--allow-net=example.com,127.0.0.1:8080,[::1]:9000"]);
        assert!(p.check_net("example.com", Some(443)).is_ok());
        assert!(p.check_net("EXAMPLE.com", None).is_ok());
        assert!(p.check_net("127.0.0.1", Some(8080)).is_ok());
        assert!(p.check_net("127.0.0.1", Some(8081)).is_err());
        assert!(p.check_net("::1", Some(9000)).is_ok());
        assert!(p.check_net("evil.com", Some(80)).is_err());
        assert!(p.check_net("sub.example.com", Some(80)).is_err());
    }

    #[test]
    fn net_entry_parse_errors() {
        assert!(NetEntry::parse("").is_err());
        assert!(NetEntry::parse("host:notaport").is_err());
        assert!(NetEntry::parse("[::1").is_err());
        assert_eq!(
            NetEntry::parse("a.b:1").unwrap(),
            NetEntry {
                host: "a.b".into(),
                port: Some(1)
            }
        );
    }

    #[test]
    fn env_and_run_lists() {
        let p = parse(&["--allow-env=HOME,PATH", "--allow-run=echo"]);
        assert!(p.check_env("HOME").is_ok());
        assert!(p.check_env("SECRET").is_err());
        assert!(p.check_env_all().is_err());
        assert!(p.check_run("echo").is_ok());
        assert!(p.check_run("/bin/echo").is_ok());
        assert!(p.check_run("rm").is_err());
    }

    #[test]
    fn flags_accumulate_and_all_absorbs() {
        let p = parse(&["--allow-read=/a", "--allow-read=/b"]);
        assert_eq!(p.read, Grant::Only(vec!["/a".into(), "/b".into()]));
        let p = parse(&["--allow-read=/a", "--allow-read"]);
        assert_eq!(p.read, Grant::All);
    }

    #[test]
    fn unknown_flags() {
        let mut p = Permissions::default();
        assert!(p.parse_flag("--allow-everything", &cwd()).is_err());
        assert!(!p.parse_flag("--other", &cwd()).unwrap());
    }

    #[test]
    fn error_message_is_helpful() {
        let e = Permissions::default()
            .check_read(Path::new("/x"), &cwd())
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "Requires read access to \"/x\", run again with the --allow-read flag"
        );
    }

    /// Property test (deterministic xorshift, no extra deps): for random
    /// paths built from `a`, `b`, `.`, `..` segments, a path is allowed under
    /// `--allow-read=/root/a` iff its lexical normal form starts with
    /// `/root/a`, and normal forms never contain `.` or `..` components.
    #[test]
    fn prop_dotdot_never_escapes() {
        let p = parse(&["--allow-read=/root/a"]);
        let mut seed: u64 = 0x9e3779b97f4a7c15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let segs = ["a", "b", ".", "..", "root"];
        for _ in 0..20_000 {
            let n = (next() % 8) as usize;
            let mut path = String::from("/root/a");
            for _ in 0..n {
                path.push('/');
                path.push_str(segs[(next() % segs.len() as u64) as usize]);
            }
            let norm = normalize_path(Path::new(&path), &cwd());
            assert!(
                norm.components()
                    .all(|c| !matches!(c, Component::CurDir | Component::ParentDir)),
                "{path} -> {norm:?}"
            );
            let allowed = p.check_read(Path::new(&path), &cwd()).is_ok();
            assert_eq!(allowed, norm.starts_with("/root/a"), "{path} -> {norm:?}");
        }
    }
}
