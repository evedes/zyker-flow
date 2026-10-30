//! The JSON files: .flow.json (read only), <work_dir>/flow.json and each feature's
//! state.json. Keys flow does not know are kept in `extra` and written back unchanged.

use crate::error::{Error, Result};
use crate::paths;
use serde::de::{DeserializeOwned, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

pub type Extra = Map<String, Value>;

#[derive(Deserialize)]
pub struct Config {
    #[serde(default = "default_work_dir")]
    pub work_dir: String,
    #[serde(default)]
    pub protected_paths: Vec<String>,
    #[serde(default)]
    pub standing_gates: Vec<String>,
    /// Commands that write code without naming the files (generators, formatters).
    #[serde(default)]
    pub codegen: Vec<String>,
    /// Commands that put the disposable environment (a dev database, say) back to match
    /// the code, run by `flow reset` and after `flow rewind`.
    #[serde(default)]
    pub reset: Vec<String>,
    #[serde(default, deserialize_with = "deploy_section")]
    pub deploy: Option<Deploy>,
    #[serde(default)]
    pub judge: Judge,
    /// Read only for its effect: with a flattened field, serde reads the config from a
    /// JSON object only, never positionally from an array.
    #[serde(flatten)]
    _rest: Extra,
}

pub fn default_work_dir() -> String {
    "flow".into()
}

fn deploy_section<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Deploy>, D::Error> {
    let section = Value::deserialize(d)?;
    let deploy = Some(&section).filter(given).map(Deploy::deserialize).transpose();
    deploy.map_err(serde::de::Error::custom)
}

/// Is a JSON value given? An empty one (null, false, 0, "", [] or {}) counts as missing
/// in flow's config and hook payloads.
pub fn given(v: &&Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[derive(Deserialize)]
pub struct Deploy {
    pub provider: Option<String>,
    pub forgejo_url: Option<String>,
    pub repo: Option<String>,
    pub token_env: Option<String>,
    pub status_command: Option<StatusCommand>,
    pub branch: Option<String>,
    pub health_url: Option<String>,
    /// A command that prints what is deployed, run before each push in ship.
    pub record: Option<String>,
    /// How to put back what `record` printed: `{before}` is its output, `{<key>}` a
    /// field of it when it is a JSON object.
    pub undo: Option<String>,
    /// As in [`Config`]: an object only.
    #[serde(flatten)]
    _rest: Extra,
}

/// How `flow judge` runs its reviewer (`claude -p`).
#[derive(Deserialize, Default)]
pub struct Judge {
    pub model: Option<String>,
    pub max_budget_usd: Option<f64>,
    /// As in [`Config`]: an object only.
    #[serde(flatten)]
    _rest: Extra,
}

/// A shell command, or an argv run without a shell.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum StatusCommand {
    Shell(String),
    Argv(Vec<String>),
}

/// A key that is there but null deserializes as `Some(None)`, so a rewrite leaves
/// a missing key missing and a null one null.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// <work_dir>/flow.json.
#[derive(Serialize, Deserialize)]
pub struct Global {
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub active: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// What flow assumes before flow.json exists.
impl Default for Global {
    fn default() -> Global {
        let extra = Extra::new();
        Global { active: Some(None), paused: Some(false), extra }
    }
}

impl Global {
    pub fn active(&self) -> Option<&str> {
        self.active.as_ref()?.as_deref().filter(|a| !a.is_empty())
    }

    pub fn paused(&self) -> bool {
        self.paused == Some(true)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Explore,
    Spec,
    Tickets,
    Build,
    Review,
    Ship,
    Done,
    Abandoned,
}

#[rustfmt::skip]
impl Phase {
    /// The phases in order (abandoned is outside it).
    pub const ALL: [Phase; 7] = [
        Phase::Explore, Phase::Spec, Phase::Tickets, Phase::Build, Phase::Review, Phase::Ship,
        Phase::Done,
    ];
    /// The phases whose artifact the user approves.
    pub const APPROVABLE: [Phase; 2] = [Phase::Spec, Phase::Tickets];

    pub fn name(self) -> &'static str {
        match self {
            Phase::Explore => "explore",
            Phase::Spec => "spec",
            Phase::Tickets => "tickets",
            Phase::Build => "build",
            Phase::Review => "review",
            Phase::Ship => "ship",
            Phase::Done => "done",
            Phase::Abandoned => "abandoned",
        }
    }

    fn step(self, by: isize) -> Option<Phase> {
        let at = Phase::ALL.iter().position(|&p| p == self)?;
        Phase::ALL.get(at.checked_add_signed(by)?).copied()
    }

    pub fn next(self) -> Option<Phase> { self.step(1) }

    pub fn prev(self) -> Option<Phase> { self.step(-1) }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// <work_dir>/<id>/state.json.
#[derive(Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub title: String,
    pub phase: Phase,
    /// By kind: spec, tickets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals: Option<BTreeMap<String, Approval>>,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub gates: Option<Option<Gates>>,
    #[serde(default)]
    pub history: Vec<Event>,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub reviewed_fingerprint: Option<Option<String>>,
    /// The gates the user lets the judge pass, by gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated: Option<BTreeMap<String, Delegation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judgements: Option<Vec<Judgement>>,
    /// What the feature did outside the repo, with how to undo it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Vec<Effect>>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl State {
    pub fn approval(&self, kind: Phase) -> Option<&Approval> {
        self.approvals.as_ref()?.get(kind.name())
    }

    pub fn gates(&self) -> Option<&Gates> {
        self.gates.as_ref()?.as_ref()
    }

    /// The user's delegation of `gate` to the judge, if they made one.
    pub fn delegated(&self, gate: Phase) -> Option<&Delegation> {
        self.delegated.as_ref()?.get(gate.name())
    }

    pub fn judgements(&self) -> &[Judgement] {
        self.judgements.as_deref().unwrap_or_default()
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct Approval {
    /// The artifact's hash (null when there was none); for review, the code's fingerprint.
    pub hash: Option<String>,
    pub at: String,
    /// "judge" when `flow judge` gave it; the user's have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// The hash of what it rests on (`Repo::basis`); none in records older than it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Gates {
    pub passed: bool,
    pub fingerprint: String,
    pub at: String,
    pub results: Vec<GateResult>,
    /// The hash of what it rests on (`Repo::basis`); none in records older than it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Serialize, Deserialize, Default)]
pub struct GateResult {
    pub source: String,
    pub command: String,
    pub exit: i32,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Delegation {
    pub at: String,
    /// How many judgements there were then: only rejections after them count.
    pub judgements: usize,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A verdict of `flow judge` on one version of a gate's artifacts.
#[derive(Serialize, Deserialize, Default)]
pub struct Judgement {
    pub gate: String,
    /// What was judged: the artifact's hash, or the code's fingerprint for review.
    pub hash: String,
    /// approve, reject or ask-user.
    pub verdict: String,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<String>,
    #[serde(default)]
    pub questions: Vec<String>,
    pub at: String,
    /// The hash of what it rests on (`Repo::basis`); none in records older than it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// An effect outside the repo: so far, a deploy.
#[derive(Serialize, Deserialize, Default)]
pub struct Effect {
    pub kind: String,
    pub at: String,
    /// The command that tells whether it is undone: it prints `before` again.
    pub record: String,
    /// What `record` printed before the effect.
    pub before: String,
    /// How to undo it, its placeholders filled in.
    pub undo: String,
    /// When `flow effects check` found it undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undone: Option<String>,
    /// When the user kept it (`flow accept`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Effect {
    /// Neither undone nor kept.
    pub fn open(&self) -> bool {
        self.undone.is_none() && self.accepted.is_none()
    }
}

/// An entry in a feature's history.
#[derive(Serialize, Deserialize)]
pub struct Event {
    pub phase: Phase,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back: Option<bool>,
    /// On a `flow rewind`: the attempt-<n> checkpoint that keeps the work it discarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewound: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Event {
    pub fn now(phase: Phase) -> Event {
        let (at, extra) = (now(), Extra::new());
        Event { phase, at, back: None, rewound: None, extra }
    }
}

/// The current UTC time, as 2026-01-31T12:00:00Z.
pub fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // The civil date from the days since 1970-01-01 (Howard Hinnant's civil_from_days).
    let z = secs / 86400 + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    let t = secs % 86400;
    let (h, m, s) = (t / 3600, t % 3600 / 60, t % 60);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_str(&paths::read_text(path)?)
        .map_err(|e| Error(format!("{}: {e}", path.display())))
}

/// Writes `data` as indented JSON through a temporary file, so no reader sees half.
pub fn write_json<T: Serialize>(path: &Path, data: &T) -> Result<()> {
    let tmp = path.with_extension("tmp");
    paths::write(&tmp, serde_json::to_string_pretty(data)? + "\n")?;
    std::fs::rename(&tmp, path).map_err(|e| paths::io_error(path, e))
}
