//! Paths and files: resolving symlinks the way the hook must, reading and writing files
//! with errors that name them, and how to name this CLI.

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

/// One step of [`resolve`]'s walk.
enum Step {
    Name(OsString),
    Parent,
    /// The parts of a symlink's target are done: what it resolves to is now known.
    Resolved(PathBuf),
}

/// The parts of `path`, last first (a stack).
fn steps(path: &Path) -> Vec<Step> {
    let mut parts: Vec<Step> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(Step::Name(name.to_owned())),
            Component::ParentDir => Some(Step::Parent),
            _ => None,
        })
        .collect();
    parts.reverse();
    parts
}

/// The absolute `path` with every symlink on it resolved, like realpath(3) but
/// non-strict: a part that does not exist is kept as written, `..` applies to what the path
/// resolved to so far, and a symlink loop stops at the link that closes it.
pub fn resolve(path: &Path) -> PathBuf {
    let mut out = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        env::current_dir().unwrap_or_else(|_| "/".into())
    };
    let mut todo = steps(path);
    // Each link met: what it resolves to, None while its target is being walked.
    let mut seen: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    while let Some(step) = todo.pop() {
        let name = match step {
            Step::Name(name) => name,
            Step::Parent => {
                out.pop();
                continue;
            }
            Step::Resolved(link) => {
                seen.insert(link, Some(out.clone()));
                continue;
            }
        };
        let next = out.join(name);
        let target = match seen.get(&next) {
            Some(Some(done)) => {
                out = done.clone();
                continue;
            }
            Some(None) => None,
            // Not a link, or not there: the part stays as it is.
            None => fs::read_link(&next).ok(),
        };
        let Some(target) = target else {
            out = next;
            continue;
        };
        if target.is_absolute() {
            out = "/".into();
        }
        seen.insert(next.clone(), None);
        todo.push(Step::Resolved(next));
        todo.extend(steps(&target));
    }
    out
}

/// A relative path as git writes it: `/`-separated, `.` when empty.
pub fn posix(path: &Path) -> String {
    let parts: Vec<_> = path.components().map(|c| c.as_os_str().to_string_lossy()).collect();
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// Is `path` the directory `dir` or inside it (both `/`-separated)?
pub fn under(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The first executable `name` on $PATH, the way the shell finds it.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
    let executable = |p: &PathBuf| {
        fs::metadata(p).is_ok_and(|m| !m.is_dir() && m.permissions().mode() & 0o111 != 0)
    };
    if path.is_empty() {
        return None;
    }
    env::split_paths(&path).map(|dir| dir.join(name)).find(executable)
}

/// Whether `path` is `me`, or a short script naming it (`exec "$HOME/.../bin/flow"`).
fn runs_me(path: &Path, me: &Path) -> bool {
    if resolve(path) == me {
        return true;
    }
    let Ok(mut f) = fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::new();
    let _ = std::io::Read::read_to_end(&mut std::io::Read::take(&mut f, 4096), &mut head);
    let text = String::from_utf8_lossy(&head);
    let words = text.split(|c: char| c.is_whitespace() || c == '\'' || c == '"');
    words.filter_map(|w| w.find("/bin/flow").map(|at| &w[..at + "/bin/flow".len()])).any(|w| {
        let expanded = expand(w);
        !expanded.contains('$') && resolve(Path::new(&expanded)) == me
    })
}

/// `~` and `$VAR` or `${VAR}` expanded from the environment; unset ones are kept.
fn expand(word: &str) -> String {
    let home = env::var("HOME").unwrap_or_default();
    let word = match word.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => word.to_string(),
    };
    let mut out = String::new();
    let mut rest = word.as_str();
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, tail) = match after.strip_prefix('{') {
            Some(b) => match b.find('}') {
                Some(end) => (&b[..end], &b[end + 1..]),
                None => ("", after),
            },
            None => {
                let end = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
                after.split_at(end.unwrap_or(after.len()))
            }
        };
        match env::var(name).ok().filter(|_| !name.is_empty()) {
            Some(v) => out.push_str(&v),
            None => out.push_str(&rest[at..rest.len() - tail.len()]),
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// How the user invokes this CLI: bare `flow` when the `flow` on PATH is this one (the
/// launcher, or a wrapper like ~/.local/bin/flow that runs it), else its path from the
/// repo root: the launcher's ($FLOW_SELF), else the binary's.
pub fn flow_cmd(root: &Path) -> String {
    let me = env::var_os("FLOW_SELF")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::current_exe().ok())
        .unwrap_or_else(|| "flow".into());
    let me = resolve(&me);
    if on_path("flow").is_some_and(|found| runs_me(&found, &me)) {
        return "flow".into();
    }
    match me.strip_prefix(resolve(root)) {
        Ok(rel) => posix(rel),
        Err(_) => me.display().to_string(),
    }
}

pub fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

pub fn io_error(path: &Path, e: std::io::Error) -> Error {
    Error(format!("{}: {e}", path.display()))
}

pub fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| io_error(path, e))
}

/// A text file with its line endings made `\n` (universal newlines).
pub fn read_text(path: &Path) -> Result<String> {
    let text = fs::read_to_string(path).map_err(|e| io_error(path, e))?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// Writes a file, creating its directory.
pub fn write(path: &Path, data: impl AsRef<[u8]>) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
    }
    fs::write(path, data).map_err(|e| io_error(path, e))
}
