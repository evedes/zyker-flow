//! The living spec: <work_dir>/specs/<area>.md, one `## <requirement>` section per
//! requirement, and the `## Spec changes` a feature's spec.md makes to it, merged at done.

use crate::error::Result;
use crate::is_space;
use crate::markdown::{lines, Doc, Fences};
use crate::paths::{posix, read_text, write};
use crate::repo::Repo;
use regex::Regex;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::LazyLock;

static ENTRY: LazyLock<Regex> = regex!(r"(?i)^(added|changed|removed)\s+([^:]*?)\s*:\s*(.*)$");
static AREA: LazyLock<Regex> = regex!(r"^[a-z0-9][a-z0-9_-]*$");

/// `text` split at its headings of `hashes` (outside code fences): the text before the
/// first, then each heading as written with its body, blank lines around both dropped.
fn split<'a>(text: &'a str, hashes: &str) -> (String, Vec<(String, String)>) {
    let mut before: Vec<&'a str> = Vec::new();
    let mut parts: Vec<(String, Vec<&'a str>)> = Vec::new();
    let mut fences = Fences::default();
    for line in lines(text) {
        let code = fences.code(line);
        let heading = line.strip_prefix(hashes).filter(|rest| rest.starts_with(is_space));
        match heading.map(|h| h.trim_matches(is_space)).filter(|h| !code && !h.is_empty()) {
            Some(h) => parts.push((h.to_string(), Vec::new())),
            None => match parts.last_mut() {
                Some((_, body)) => body.push(line),
                None => before.push(line),
            },
        }
    }
    let parts = parts.into_iter().map(|(h, body)| (h, unpad(&body))).collect();
    (unpad(&before), parts)
}

/// Lines joined, without the blank ones at either end.
fn unpad(lines: &[&str]) -> String {
    let blank = |l: &&str| l.trim_matches(is_space).is_empty();
    let start = lines.iter().position(|l| !blank(l)).unwrap_or(lines.len());
    let end = lines.iter().rposition(|l| !blank(l)).map_or(start, |e| e + 1);
    lines[start..end].join("\n")
}

/// Requirement names match whatever their case and spacing.
fn key(name: &str) -> String {
    name.split(is_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ").to_lowercase()
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Added,
    Changed,
    Removed,
}

/// One `### <op> <area>: <requirement>` entry of `## Spec changes`.
struct Change {
    heading: String,
    op: Op,
    area: String,
    name: String,
    body: String,
}

/// The entries of a `## Spec changes` section, or what is wrong with them.
fn parse_changes(section: &str, problems: &mut Vec<String>) -> Vec<Change> {
    let (before, entries) = split(section, "###");
    if entries.is_empty() {
        let none = before.trim_end_matches('.').eq_ignore_ascii_case("none");
        if !before.is_empty() && !none {
            problems.push(
                "spec.md: '## Spec changes' has no `### Added|Changed|Removed <area>: \
                 <requirement>` entries; write \"None.\" if it changes none"
                    .into(),
            );
        }
        return Vec::new();
    }
    let mut changes = Vec::new();
    for (heading, body) in entries {
        let label = format!("spec.md: '### {heading}'");
        let Some(m) = ENTRY.captures(&heading) else {
            problems.push(format!("{label}: not `Added|Changed|Removed <area>: <requirement>`"));
            continue;
        };
        let op = match m[1].to_lowercase().as_str() {
            "added" => Op::Added,
            "changed" => Op::Changed,
            _ => Op::Removed,
        };
        let (area, name) = (m[2].to_string(), m[3].trim_matches(is_space).to_string());
        if !AREA.is_match(&area) {
            let why = "is not an area name (lowercase letters, digits, - and _)";
            problems.push(format!("{label}: '{area}' {why}"));
        } else if name.is_empty() {
            problems.push(format!("{label}: no requirement named after the ':'"));
        } else if op != Op::Removed && body.is_empty() {
            problems.push(format!("{label}: the requirement's text is missing"));
        } else if changes.iter().any(|c: &Change| c.area == area && key(&c.name) == key(&name)) {
            problems.push(format!("{label}: {area}: {name} has more than one entry"));
        } else {
            changes.push(Change { heading, op, area, name, body });
        }
    }
    changes
}

/// What merging a feature's spec changes would write, or why they don't apply.
pub struct Merge {
    pub writes: Vec<(PathBuf, String)>,
    pub problems: Vec<String>,
}

/// The living spec's area files once the feature's spec changes are made to them.
pub fn merge(repo: &Repo, fid: &str) -> Result<Merge> {
    let mut problems = Vec::new();
    let changes = match Doc::load(&repo.fdir(fid).join("spec.md"))? {
        Some(doc) => parse_changes(doc.sections.get("spec changes"), &mut problems),
        None => Vec::new(),
    };
    let mut by_area: BTreeMap<&str, Vec<&Change>> = BTreeMap::new();
    for c in &changes {
        by_area.entry(&c.area).or_default().push(c);
    }
    let mut writes = Vec::new();
    for (area, changes) in by_area {
        let path = repo.work.join("specs").join(format!("{area}.md"));
        let rel = format!("{}/specs/{area}.md", repo.work_rel);
        let old = if path.is_file() { Some(read_text(&path)?) } else { None };
        let (title, mut reqs) = match &old {
            Some(text) => split(text, "##"),
            None => (format!("# {area}"), Vec::new()),
        };
        for c in changes {
            let label = format!("spec.md: '### {}'", c.heading);
            let at: Vec<usize> =
                (0..reqs.len()).filter(|&i| key(&reqs[i].0) == key(&c.name)).collect();
            match (c.op, at.as_slice()) {
                (Op::Added, []) => reqs.push((c.name.clone(), c.body.clone())),
                (Op::Added, _) => problems.push(format!("{label}: {rel} already has it")),
                (_, []) => problems.push(format!("{label}: {rel} has no requirement '{}'", c.name)),
                (Op::Changed, [i]) => reqs[*i] = (c.name.clone(), c.body.clone()),
                (Op::Removed, [i]) => {
                    reqs.remove(*i);
                }
                (_, _) => problems.push(format!("{label}: {rel} has it more than once")),
            }
        }
        let mut parts: Vec<String> = Vec::new();
        parts.extend(Some(title).filter(|t| !t.is_empty()));
        for (name, body) in reqs {
            parts.push(format!("## {name}"));
            parts.extend(Some(body).filter(|b| !b.is_empty()));
        }
        let text = parts.join("\n\n") + "\n";
        if old.as_ref() != Some(&text) {
            writes.push((path, text));
        }
    }
    if !problems.is_empty() {
        writes.clear();
    }
    Ok(Merge { writes, problems })
}

/// Merges the feature's spec changes into the living spec; the files it wrote, from the root.
pub fn apply(repo: &Repo, merge: Merge) -> Result<Vec<String>> {
    let mut written = Vec::new();
    for (path, text) in merge.writes {
        write(&path, text)?;
        written
            .push(path.strip_prefix(&repo.root).map_or_else(|_| path.display().to_string(), posix));
    }
    Ok(written)
}
