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

/// An absolute path as text, `/`-separated.
fn posix_abs(path: &Path) -> String {
    path.to_string_lossy().trim_end_matches('/').to_string()
}

/// `s` in quotes, as flow's messages quote a pattern: single quotes, or double ones when
/// `s` has a single quote and no double one; backslashes and that quote escaped.
fn quote(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let body = s.replace('\\', "\\\\").replace(q, &format!("\\{q}"));
    format!("{q}{body}{q}")
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
        let protected = config.protected_paths.iter();
        let protected = protected.map(|p| p.trim_end_matches('/').to_string()).collect();
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
    /// written, or after `./`, `$PWD/` or the root's absolute path, and ending there or at
    /// a `/`. So `/tmp/lib/x` doesn't name `lib/`, nor `src-old` `src`.
    pub fn names(&self, alts: &str) -> Result<regex::Regex> {
        let mut roots = BTreeSet::from([posix_abs(&self.root), posix_abs(&self.real_root)]);
        let home = std::env::var("HOME").ok().filter(|h| h.starts_with('/'));
        if let Some(home) = home.map(|h| h.trim_end_matches('/').to_string()) {
            let tilde = roots.iter().filter_map(|r| r.strip_prefix(&home)).map(|r| format!("~{r}"));
            let tilde: Vec<String> = tilde.filter(|r| r.starts_with("~/")).collect();
            roots.extend(tilde);
        }
        let mut prefixes = vec![r"\./".to_string(), r"\$PWD/".into(), r"\$\{PWD\}/".into()];
        prefixes.push(r"\$\(pwd\)/".into());
        prefixes.extend(roots.iter().map(|r| regex::escape(&format!("{r}/"))));
        let (space, prefix) = (r"\s\x1c-\x1f", prefixes.join("|"));
        let re = format!(r#"(?:^|[{space}'"=])(?:{prefix})*(?:{alts})(?:[/*?{space}'";&|()<>]|$)"#);
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

    pub fn approval_valid(&self, fid: &str, st: &State, kind: Phase) -> Result<bool> {
        match st.approval(kind) {
            Some(a) => Ok(a.hash == self.artifact_hash(fid, kind)?),
            None => Ok(false),
        }
    }

    pub fn gates_current(&self, st: &State) -> Result<bool> {
        match st.gates() {
            Some(g) if g.passed => Ok(g.fingerprint == self.fingerprint()?),
            _ => Ok(false),
        }
    }
}
