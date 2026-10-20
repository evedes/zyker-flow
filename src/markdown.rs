//! The checks flow makes on its markdown artifacts.

use crate::error::Result;
use crate::is_space;
use crate::paths::read_text;
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

pub const PLACEHOLDER: &str = "TODO(flow)";

static UNCHECKED_BOX: LazyLock<Regex> = regex!(r"(?m)^[\s\x1c-\x1f]*[-*][\s\x1c-\x1f]+\[ \]");

/// A document's lines. Besides \n, \v, \f, \x1c-\x1e, NEL and U+2028/2029 end a line;
/// the separators \x1c-\x1f count as whitespace ([`is_space`]).
pub fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c| {
        matches!(c, '\n' | '\x0b' | '\x0c' | '\x1c'..='\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}')
    })
}

/// Whether lines are inside a fenced code block: one opened by ``` or ~~~ and closed by
/// the same, its own fence lines included.
#[derive(Default)]
pub struct Fences(Option<char>);

impl Fences {
    pub fn code(&mut self, line: &str) -> bool {
        let start = line.trim_start_matches(is_space);
        let fence = ['`', '~'].into_iter().find(|&c| start.chars().take(3).all(|s| s == c));
        let fence = fence.filter(|_| start.len() >= 3);
        match (self.0, fence) {
            (None, Some(c)) => self.0 = Some(c),
            (Some(open), Some(c)) if open == c => self.0 = None,
            (open, _) => return open.is_some(),
        }
        true
    }
}

/// A document's `## ` sections in order: lowercased heading, trimmed body. A repeated
/// heading keeps its first place and its last body. A `## ` line in a code fence is text.
pub struct Sections(Vec<(String, String)>);

impl Sections {
    pub fn parse(text: &str) -> Sections {
        let mut out: Vec<(String, Vec<&str>)> = Vec::new();
        let mut cur: Option<usize> = None;
        let mut fences = Fences::default();
        for line in lines(text) {
            let code = fences.code(line);
            let Some(key) = heading(line).filter(|_| !code) else {
                if let Some(i) = cur {
                    out[i].1.push(line);
                }
                continue;
            };
            let i = out.iter().position(|(k, _)| *k == key).unwrap_or_else(|| {
                out.push((key, Vec::new()));
                out.len() - 1
            });
            out[i].1.clear();
            cur = Some(i);
        }
        let trimmed = out
            .into_iter()
            .map(|(k, body)| (k, body.join("\n").trim_matches(is_space).to_string()));
        Sections(trimmed.collect())
    }

    pub fn read(path: &Path) -> Result<Sections> {
        Ok(Sections::parse(&read_text(path)?))
    }

    pub fn get(&self, key: &str) -> &str {
        self.0.iter().find(|(k, _)| k == key).map_or("", |(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = &(String, String)> {
        self.0.iter()
    }
}

/// `## Heading` (whitespace after the hashes, then some text) gives "heading".
fn heading(line: &str) -> Option<String> {
    let rest = line.strip_prefix("##")?;
    let mut chars = rest.chars();
    let ok = is_space(chars.next()?) && chars.next().is_some();
    ok.then(|| rest.trim_matches(is_space).to_lowercase())
}

/// A markdown artifact, read once.
pub struct Doc {
    pub text: String,
    pub sections: Sections,
}

impl Doc {
    /// The document at `path`; None when it is not a file.
    pub fn load(path: &Path) -> Result<Option<Doc>> {
        if !path.is_file() {
            return Ok(None);
        }
        let text = read_text(path)?;
        Ok(Some(Doc { sections: Sections::parse(&text), text }))
    }
}

pub fn check_doc(doc: Option<&Doc>, required: &[String], label: &str) -> Vec<String> {
    let Some(doc) = doc else {
        return vec![format!("{label}: missing")];
    };
    let mut problems: Vec<String> = required
        .iter()
        .filter(|s| doc.sections.get(s).is_empty())
        .map(|s| format!("{label}: section '## {s}' is missing or empty"))
        .collect();
    if doc.text.contains(PLACEHOLDER) {
        problems.push(format!("{label}: still contains {PLACEHOLDER} placeholders"));
    }
    problems
}

pub fn unchecked_boxes(secs: &Sections, section: &str, name: &str) -> Option<String> {
    let n = UNCHECKED_BOX.find_iter(secs.get(section)).count();
    (n > 0).then(|| format!("{name}: {n} unanswered item(s) in '## {section}'"))
}

/// Commands in the fenced code block(s) of a ticket's Verification steps.
pub fn gate_commands(ticket: &Sections) -> Vec<String> {
    let mut inside = false;
    let mut cmds = Vec::new();
    for line in lines(ticket.get("verification steps")).map(|l| l.trim_matches(is_space)) {
        if line.starts_with("```") {
            inside = !inside;
        } else if inside && !line.is_empty() && !line.starts_with('#') {
            cmds.push(line.to_string());
        }
    }
    cmds
}
