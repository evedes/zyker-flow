//! A repository using flow: its config, state files and fingerprints.

use crate::config::{read_json, write_json, Config, Global, Phase, State};
use crate::error::{bail, Error, Result};
use crate::markdown::{Sections, PLACEHOLDER};
use crate::paths::{file_name, flow_cmd, posix, read, read_text, resolve, under};
use fancy_regex::{Regex, RegexBuilder};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The plugin's own templates, built in.
fn builtin_template(name: &str) -> Option<&'static str> {
    Some(match name {
        "research.md" => include_str!("../templates/research.md"),
        "spec.md" => include_str!("../templates/spec.md"),
        "ticket.md" => include_str!("../templates/ticket.md"),
        "review.md" => include_str!("../templates/review.md"),
        "STANDING.md" => include_str!("../templates/STANDING.md"),
        _ => return None,
    })
}

/// The first 16 hex digits of a SHA-256.
fn hex16(digest: &[u8]) -> String {
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn run_git(dir: &Path, args: &[&str]) -> Result<Output> {
    let out = Command::new("git").args(args).current_dir(dir).output();
    out.map_err(|e| Error(format!("git {} in {}: {e}", args.join(" "), dir.display())))
}

/// Runs git in `dir` for its stdout, with line endings made `\n`.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = run_git(dir, args)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    let text = String::from_utf8(out.stdout).map_err(|_| "git printed non-UTF-8 output")?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

pub fn git_root(start: &Path) -> Result<Option<PathBuf>> {
    let out = run_git(start, &["rev-parse", "--show-toplevel"])?;
    let root = String::from_utf8_lossy(&out.stdout);
    Ok(out.status.success().then(|| PathBuf::from(root.trim())))
}

/// The git root of `start`, when it opts in to flow with a .flow.json.
pub fn find_root(start: &Path) -> Result<Option<PathBuf>> {
    Ok(git_root(start)?.filter(|root| root.join(".flow.json").is_file()))
}

/// The flow repo `path` is in, symlinks resolved and from its nearest existing directory:
/// the innermost git root around it with a .flow.json, past nested repos without one (a
/// dependency checked out inside the tree).
pub fn flow_root_of(path: &Path) -> Result<Option<PathBuf>> {
    let mut dir = resolve(path);
    while !dir.is_dir() {
        if !dir.pop() {
            return Ok(None);
        }
    }
    while let Some(root) = git_root(&dir)? {
        if root.join(".flow.json").is_file() {
            return Ok(Some(root));
        }
        match root.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    Ok(None)
}

/// `s` in quotes, as flow's messages quote a pattern: single quotes, or double ones when
/// `s` has a single quote and no double one; backslashes and that quote escaped.
fn quote(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let body = s.replace('\\', "\\\\").replace(q, &format!("\\{q}"));
    format!("{q}{body}{q}")
}

/// A protected path as [`Repo::is_protected`] compares it, `/`-separated without `.`, empty
/// parts or a trailing `/` (`./lib/` is `lib`). One that would protect nothing or the whole
/// repo without saying so is an error.
fn protected_path(p: &str) -> Result<String> {
    let parts: Vec<&str> = p.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    let why = if p.starts_with('/') {
        "is absolute; protected paths are relative to the git root"
    } else if parts.contains(&"..") {
        "leaves the repo"
    } else if p.contains(['*', '?', '[']) {
        "is a glob; protected paths are path prefixes"
    } else if parts.is_empty() {
        "names the whole repo"
    } else {
        return Ok(parts.join("/"));
    };
    bail!(".flow.json protected path {}: {why}", quote(p))
}

pub struct Repo {
    pub root: PathBuf,
    real_root: PathBuf,
    pub config: Config,
    pub work: PathBuf,
    pub work_rel: String,
    pub protected: Vec<String>,
    pub codegen: Vec<Regex>,
}

impl Repo {
    pub fn open(root: &Path) -> Result<Repo> {
        let config: Config = read_json(&root.join(".flow.json"))?;
        let work = root.join(&config.work_dir);
        let Ok(work_rel) = work.strip_prefix(root).map(posix) else {
            bail!("work_dir {} is not inside {}", config.work_dir, root.display());
        };
        let protected = config.protected_paths.iter().map(|p| protected_path(p));
        let protected = protected.collect::<Result<_>>()?;
        // No backtracking limit: a long command is judged, not failed.
        let compile = |p: &String| {
            RegexBuilder::new(p)
                .backtrack_limit(usize::MAX)
                .build()
                .map_err(|e| Error(format!(".flow.json codegen pattern {}: {e}", quote(p))))
        };
        let codegen = config.codegen.iter().map(compile).collect::<Result<_>>()?;
        let (root, real_root) = (root.to_path_buf(), resolve(root));
        Ok(Repo { root, real_root, config, work, work_rel, protected, codegen })
    }

    /// The repo's own template in <work_dir>/templates/, else the plugin's.
    pub fn template(&self, name: &str) -> Result<String> {
        let own = self.work.join("templates").join(name);
        if own.is_file() {
            return read_text(&own);
        }
        let builtin = builtin_template(name).ok_or_else(|| format!("no template {name}"))?;
        Ok(builtin.to_string())
    }

    /// The sections a template seeds with a placeholder must be filled in.
    pub fn required_sections(&self, name: &str) -> Result<Vec<String>> {
        let secs = Sections::parse(&self.template(name)?);
        let seeded = secs.iter().filter(|(_, body)| body.contains(PLACEHOLDER));
        Ok(seeded.map(|(s, _)| s.clone()).collect())
    }

    pub fn load_global(&self) -> Result<Global> {
        let p = self.work.join("flow.json");
        if p.is_file() {
            read_json(&p)
        } else {
            Ok(Global::default())
        }
    }

    pub fn save_global(&self, g: &Global) -> Result<()> {
        write_json(&self.work.join("flow.json"), g)
    }

    /// Sets (or clears) the active feature.
    pub fn set_active(&self, fid: Option<&str>) -> Result<()> {
        let mut g = self.load_global()?;
        g.active = Some(fid.map(String::from));
        self.save_global(&g)
    }

    pub fn fdir(&self, fid: &str) -> PathBuf {
        self.work.join(fid)
    }

    pub fn save_state(&self, fid: &str, st: &State) -> Result<()> {
        write_json(&self.fdir(fid).join("state.json"), st)
    }

    /// The active feature's id and state, if there is one.
    pub fn active(&self) -> Result<Option<(String, State)>> {
        let g = self.load_global()?;
        let Some(fid) = g.active() else {
            return Ok(None);
        };
        let path = self.fdir(fid).join("state.json");
        if !path.is_file() {
            let (work, cmd) = (&self.work_rel, flow_cmd(&self.root));
            bail!(
                "active feature {fid} has no {work}/{fid}/state.json (removed by `git clean` or \
                 `git stash`?): restore it, or the user can `! {cmd} abandon` to clear it"
            );
        }
        Ok(Some((fid.to_string(), read_json(&path)?)))
    }

    pub fn require_active(&self) -> Result<(String, State)> {
        self.active()?.ok_or_else(|| {
            let cmd = flow_cmd(&self.root);
            Error(format!("no active feature; start one with `{cmd} new <slug> \"<title>\"`"))
        })
    }

    /// `path` (absolute, or relative to the root) relative to the root once resolved;
    /// None when it is outside.
    pub fn rel(&self, path: &str) -> Option<String> {
        if path.contains('\0') {
            return None;
        }
        let full = resolve(&self.root.join(path));
        full.strip_prefix(&self.real_root).ok().map(posix)
    }

    /// A regex for a Bash word naming one of `alts` (repo-relative paths, as regex): as
    /// written, or after `./` or `$PWD/`, and ending there or at a `/`. So `/tmp/lib/x`
    /// doesn't name `lib/`, nor `src-old` `src`. Absolute words are resolved instead (see
    /// [`crate::shell::absolute_words`]). After `=` it is a path as an option's value
    /// (`--target=lib`) or when it goes on with `/` (`x=lib/y`); `MIX_ENV=test` is a
    /// value, not `test/`.
    pub fn names(&self, alts: &str) -> Result<regex::Regex> {
        let prefix = r"\./|\$PWD/|\$\{PWD\}/|\$\(pwd\)/";
        let space = r"\s\x1c-\x1f";
        let path = format!(r#"(?:{prefix})*(?:{alts})(?:[/*?{space}'";&|()<>]|$)"#);
        let start = format!(r#"(?:^|[{space}'"])"#);
        let option = format!(r#"{start}-[^{space}'"=]*="#);
        let re = format!(r"{start}{path}|{option}{path}|=(?:{prefix})*(?:{alts})/");
        Ok(regex::Regex::new(&re)?)
    }

    pub fn is_protected(&self, rel: &str) -> bool {
        self.protected.iter().any(|p| under(rel, p))
    }

    pub fn is_state_file(&self, rel: &str) -> bool {
        let Some(r) = rel.strip_prefix(&self.work_rel).and_then(|r| r.strip_prefix('/')) else {
            return false;
        };
        let id = r.strip_suffix("/state.json");
        r == "flow.json" || id.is_some_and(|id| !id.is_empty() && !id.contains('/'))
    }

    /// .flow.json, .claude/settings.json and settings.local.json (the hooks and deny rules),
    /// <work_dir>/STANDING.md, <work_dir>/templates/ and <work_dir>/specs/: what a feature
    /// is judged by.
    pub fn is_rule_file(&self, rel: &str) -> bool {
        let work = |p: &str| format!("{}/{p}", self.work_rel);
        rel == ".flow.json"
            || rel == ".claude/settings.json"
            || rel == ".claude/settings.local.json"
            || rel == work("STANDING.md")
            || under(rel, &work("templates"))
            || under(rel, &work("specs"))
    }

    /// A content fingerprint of everything git would see, minus the work dir: each path,
    /// then its content, a symlink's target or `<deleted>`.
    pub fn fingerprint(&self) -> Result<String> {
        let out = git(&self.root, &["ls-files", "-co", "--exclude-standard", "-z"])?;
        let files: BTreeSet<&str> = out.split('\0').filter(|f| !f.is_empty()).collect();
        let mut h = Sha256::new();
        for f in files.into_iter().filter(|f| !under(f, &self.work_rel)) {
            let p = self.root.join(f);
            h.update(f.as_bytes());
            h.update(b"\0");
            if p.is_symlink() {
                let target = fs::read_link(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                let target = target
                    .to_str()
                    .ok_or_else(|| format!("{}: the symlink target is not UTF-8", p.display()))?;
                h.update(target.as_bytes());
            } else if p.is_file() {
                h.update(read(&p)?);
            } else {
                h.update(b"<deleted>");
            }
            h.update(b"\0");
        }
        Ok(hex16(&h.finalize()))
    }

    /// Whether any file git sees (tracked, or untracked and not ignored) is protected code.
    pub fn has_code(&self) -> Result<bool> {
        let out = git(&self.root, &["ls-files", "-co", "--exclude-standard", "-z"])?;
        Ok(out.split('\0').any(|f| !f.is_empty() && self.is_protected(f)))
    }

    /// The paths `git status` lists, both sides of a rename or copy.
    pub fn changed_paths(&self) -> Result<Vec<String>> {
        let out = git(&self.root, &["status", "--porcelain", "-z", "--untracked-files=all"])?;
        let mut paths = Vec::new();
        let mut entries = out.split('\0');
        while let Some(e) = entries.next() {
            let Some(path) = e.get(3..).filter(|p| !p.is_empty()) else {
                continue;
            };
            paths.push(path.to_string());
            if e.starts_with(['R', 'C']) {
                paths.extend(entries.next().filter(|s| !s.is_empty()).map(String::from));
            }
        }
        Ok(paths)
    }

    /// The paths staged for the next commit.
    pub fn staged_paths(&self) -> Result<Vec<String>> {
        let out = git(&self.root, &["diff", "--cached", "--name-only", "--no-renames", "-z"])?;
        Ok(out.split('\0').filter(|p| !p.is_empty()).map(String::from).collect())
    }

    /// A feature's tickets/*.md, by name.
    pub fn tickets(&self, fid: &str) -> Vec<PathBuf> {
        let dir = fs::read_dir(self.fdir(fid).join("tickets"));
        let mut files: Vec<PathBuf> =
            dir.into_iter().flatten().flatten().map(|e| e.path()).collect();
        files.retain(|p| p.to_str().is_some_and(|s| s.ends_with(".md")));
        files.sort();
        files
    }

    /// The hash an approval of `kind` records: spec.md's, or all the tickets'.
    pub fn artifact_hash(&self, fid: &str, kind: Phase) -> Result<Option<String>> {
        if kind == Phase::Spec {
            let spec = self.fdir(fid).join("spec.md");
            let content = spec.is_file().then(|| read(&spec)).transpose()?;
            return Ok(content.map(|c| hex16(&Sha256::digest(c))));
        }
        let files = self.tickets(fid);
        if files.is_empty() {
            return Ok(None);
        }
        let mut h = Sha256::new();
        for f in files {
            h.update(file_name(&f).as_bytes());
            h.update(b"\0");
            h.update(read(&f)?);
        }
        Ok(Some(hex16(&h.finalize())))
    }

    /// What an approval of `kind` is bound to: its artifact's hash, or for review the
    /// code's fingerprint.
    pub fn gate_hash(&self, fid: &str, kind: Phase) -> Result<Option<String>> {
        match kind {
            Phase::Review => Ok(Some(self.fingerprint()?)),
            _ => self.artifact_hash(fid, kind),
        }
    }

    /// What an approval of `kind`, or the gates (`Phase::Build`), rests on: the spec for
    /// the tickets; the spec and the tickets for the gates and the review. None for the
    /// spec, which rests on nothing flow approves.
    pub fn basis(&self, fid: &str, kind: Phase) -> Result<Option<String>> {
        let spec = self.artifact_hash(fid, Phase::Spec)?;
        match kind {
            Phase::Tickets => Ok(spec),
            Phase::Build | Phase::Review => {
                let tickets = self.artifact_hash(fid, Phase::Tickets)?;
                let mut h = Sha256::new();
                h.update(spec.unwrap_or_default());
                h.update(b"\0");
                h.update(tickets.unwrap_or_default());
                Ok(Some(hex16(&h.finalize())))
            }
            _ => Ok(None),
        }
    }

    /// Whether a record made on `basis` still rests on what it did. One without a basis
    /// predates them, and is taken as it was.
    fn rests(&self, fid: &str, kind: Phase, basis: &Option<String>) -> Result<bool> {
        Ok(basis.is_none() || *basis == self.basis(fid, kind)?)
    }

    pub fn approval_valid(&self, fid: &str, st: &State, kind: Phase) -> Result<bool> {
        match st.approval(kind) {
            Some(a) => Ok(a.hash == self.gate_hash(fid, kind)? && self.rests(fid, kind, &a.basis)?),
            None => Ok(false),
        }
    }

    /// Whether `kind`'s approval still matches its own artifact but no longer what it
    /// rests on (the tickets, approved on another spec).
    pub fn approval_outdated(&self, fid: &str, st: &State, kind: Phase) -> Result<bool> {
        match st.approval(kind) {
            Some(a) => {
                Ok(a.hash == self.gate_hash(fid, kind)? && !self.rests(fid, kind, &a.basis)?)
            }
            None => Ok(false),
        }
    }

    /// Whether the gates passed on the code as it is now, and on the spec and tickets.
    pub fn gates_current(&self, fid: &str, st: &State) -> Result<bool> {
        match st.gates() {
            Some(g) if g.passed => {
                Ok(g.fingerprint == self.fingerprint()?
                    && self.rests(fid, Phase::Build, &g.basis)?)
            }
            _ => Ok(false),
        }
    }
}
