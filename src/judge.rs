//! `flow judge`: a fresh, read-only Claude (`claude -p`) that passes a gate the user
//! delegated to it, on a brief flow fixes (src/judge.md) and the gate's artifacts.

use crate::config::{Phase, State};
use crate::deploy::finish;
use crate::error::{bail, Error, Result};
use crate::markdown::Sections;
use crate::paths::{file_name, read_text};
use crate::repo::{git, Repo};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The gates the user can delegate to the judge.
pub const GATES: [Phase; 3] = [Phase::Spec, Phase::Tickets, Phase::Review];
/// Rejections of one gate, since it was delegated, before the judge hands it back.
pub const MAX_REJECTS: usize = 3;
/// The most of the diff the brief carries; the judge reads the rest itself.
const MAX_DIFF: usize = 200_000;
const BRIEF: &str = include_str!("judge.md");
/// The judge reads; it never writes or runs anything.
const TOOLS: &str = "Read,Grep,Glob";
/// How long a run may take by default: inside the Bash tool's ten-minute limit, so the
/// agent's call ends with flow's error rather than the tool's.
const TIMEOUT_SECS: u64 = 540;

pub const VERDICTS: [&str; 3] = ["approve", "reject", "ask-user"];

#[derive(Deserialize)]
pub struct Verdict {
    pub verdict: String,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<String>,
    #[serde(default)]
    pub questions: Vec<String>,
}

fn schema() -> Value {
    let strings = json!({"type": "array", "items": {"type": "string"}});
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "summary", "findings", "questions"],
        "properties": {
            "verdict": {"enum": VERDICTS},
            "summary": {"type": "string"},
            "findings": strings,
            "questions": strings,
        },
    })
}

/// `text` in a tag the brief explains: who wrote it, and where it comes from.
fn tagged(tag: &str, path: &str, text: &str) -> String {
    format!("<{tag} path=\"{path}\">\n{}\n</{tag}>\n\n", text.trim_end())
}

/// What the judge reads for `gate`, as the brief's {artifacts}.
fn artifacts(repo: &Repo, fid: &str, st: &State, gate: Phase) -> Result<String> {
    let (d, work) = (repo.fdir(fid), &repo.work_rel);
    let mut out = String::new();
    let mut own = |name: &str, path: &std::path::Path| -> Result<()> {
        if path.is_file() {
            out += &tagged("artifact", &format!("{work}/{fid}/{name}"), &read_text(path)?);
        }
        Ok(())
    };
    own("research.md", &d.join("research.md"))?;
    own("spec.md", &d.join("spec.md"))?;
    if gate != Phase::Spec {
        for t in repo.tickets(fid) {
            own(&format!("tickets/{}", file_name(&t)), &t)?;
        }
    }
    let standing = repo.work.join("STANDING.md");
    if gate != Phase::Spec && standing.is_file() {
        out += &tagged("rules", &format!("{work}/STANDING.md"), &read_text(&standing)?);
    }
    if gate == Phase::Review {
        let gates = st.gates().map_or_else(Vec::new, |g| {
            let line = |r: &crate::config::GateResult| {
                format!("[{}] {} => exit {}", r.source, r.command, r.exit)
            };
            g.results.iter().map(line).collect()
        });
        out += &tagged("flow", "gate results", &gates.join("\n"));
        let exclude = format!(":(exclude){work}");
        let mut diff =
            git(&repo.root, &["diff", "--no-color", "--no-ext-diff", "HEAD", "--", ".", &exclude])?;
        if diff.len() > MAX_DIFF {
            let cut = (0..=MAX_DIFF).rev().find(|&i| diff.is_char_boundary(i)).unwrap_or(0);
            diff.truncate(cut);
            diff += "\n[diff cut short: read the changed files themselves]";
        }
        out += &tagged("artifact", "git diff HEAD", &diff);
        let new = git(&repo.root, &["ls-files", "-o", "--exclude-standard", "--", ".", &exclude])?;
        out += &tagged("flow", "new files (not in the diff: read them)", &new);
    }
    Ok(out)
}

/// The prompt: the brief with the gate's criteria and artifacts.
pub fn prompt(repo: &Repo, fid: &str, st: &State, gate: Phase) -> Result<String> {
    let brief = Sections::parse(BRIEF);
    let text = brief.get("brief").replace("{criteria}", brief.get(gate.name()));
    let text = text.replace("{gate}", gate.name()).replace("{work_dir}", &repo.work_rel);
    let text = text.replace("{feature}", &format!("{fid} ({})", st.title));
    Ok(text.replace("{artifacts}", &artifacts(repo, fid, st, gate)?))
}

/// Runs the judge on `prompt` for its verdict.
pub fn ask(repo: &Repo, prompt: &str) -> Result<Verdict> {
    let mut c = Command::new("claude");
    c.args(["-p", "--output-format", "json", "--no-session-persistence"]);
    c.args(["--tools", TOOLS, "--allowedTools", TOOLS, "--json-schema", &schema().to_string()]);
    // Nothing but those tools: no MCP servers, skills or permission mode of the user's,
    // and none of the repo's own settings (hooks, permissions), which the agent can write.
    c.args(["--strict-mcp-config", "--disable-slash-commands", "--permission-mode", "dontAsk"]);
    c.args(["--setting-sources", "user"]);
    if let Some(model) = &repo.config.judge.model {
        c.args(["--model", model]);
    }
    if let Some(budget) = repo.config.judge.max_budget_usd {
        c.args(["--max-budget-usd", &budget.to_string()]);
    }
    c.current_dir(&repo.root).stdin(Stdio::piped()).stdout(Stdio::piped());
    let cant = |e| Error(format!("cannot run claude, which `flow judge` runs as the judge: {e}"));
    let mut child = c.spawn().map_err(cant)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(prompt.as_bytes())?;
    }
    let secs = repo.config.judge.timeout_secs.unwrap_or(TIMEOUT_SECS);
    let Some((status, [stdout, _])) = finish(child, Duration::from_secs(secs))? else {
        bail!("the judge took longer than {secs} seconds; nothing recorded");
    };
    let reply: Value = serde_json::from_slice(&stdout).map_err(|e| {
        let stdout = String::from_utf8_lossy(&stdout);
        Error(format!("the judge's reply is not JSON ({e}): {}", stdout.trim()))
    })?;
    let failed = reply.get("is_error").and_then(Value::as_bool) != Some(false);
    if failed || !status.success() {
        let what = reply.get("subtype").and_then(Value::as_str).unwrap_or("error");
        let result = reply.get("result").and_then(Value::as_str).unwrap_or("");
        bail!("the judge failed ({what}): {}", result.trim());
    }
    let verdict = reply.get("structured_output").cloned().unwrap_or(Value::Null);
    let verdict: Verdict = serde_json::from_value(verdict)
        .map_err(|e| Error(format!("the judge gave no verdict ({e})")))?;
    if !VERDICTS.contains(&verdict.verdict.as_str()) {
        bail!("the judge's verdict {:?} is not one of {}", verdict.verdict, VERDICTS.join(", "));
    }
    Ok(verdict)
}
