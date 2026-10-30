//! The effect ledger: what a feature did outside the repo (so far, a deploy), with how
//! to undo it and a command that tells whether it is undone. A deploy is recorded by the
//! push hook in ship; `flow effects` shows the ledger, renders it as a prompt and checks
//! it; `flow abandon` and `flow rewind` wait until each effect is undone or kept.

use crate::config::{now, Effect, Phase, State};
use crate::deploy::run_output;
use crate::error::{bail, Result};
use crate::paths::flow_cmd;
use crate::repo::Repo;
use serde_json::Value;
use std::process::Command;

/// Runs a record command: what it printed, trimmed.
fn record(repo: &Repo, command: &str) -> Result<String> {
    let mut sh = Command::new("/bin/sh");
    sh.arg("-c").arg(command);
    let out = run_output(sh, &repo.root, &format!("`{command}`"))?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Do two outputs of a record command say the same? JSON is compared as JSON, so key
/// order and spacing don't count.
fn same(a: &str, b: &str) -> bool {
    match (serde_json::from_str::<Value>(a), serde_json::from_str::<Value>(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `undo` with `{before}` and, when `before` is a JSON object, `{<key>}` filled in.
fn render(undo: &str, before: &str) -> String {
    let mut text = undo.replace("{before}", before);
    if let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(before) {
        for (key, value) in fields {
            let value = match value {
                Value::String(s) => s,
                v => v.to_string(),
            };
            text = text.replace(&format!("{{{key}}}"), &value);
        }
    }
    text
}

/// Before a push: in ship, with `deploy.record` set, records what is deployed, unless
/// the feature already has a deploy to undo (undoing that one puts back the first).
/// Why the push is refused, if it is: when what is deployed can't be recorded.
pub fn before_push(repo: &Repo) -> Result<Option<String>> {
    let Some(deploy) = &repo.config.deploy else { return Ok(None) };
    let Some(command) = deploy.record.as_deref().filter(|c| !c.is_empty()) else {
        return Ok(None);
    };
    let Some((fid, mut st)) = repo.active()? else { return Ok(None) };
    if st.phase != Phase::Ship || open(&st).any(|(_, e)| e.kind == "deploy") {
        return Ok(None);
    }
    let before = match record(repo, command) {
        Ok(before) => before,
        Err(e) => {
            let cmd = flow_cmd(&repo.root);
            return Ok(Some(format!(
                "cannot record what is deployed before the push, so it couldn't be undone \
                 ({e}); fix deploy.record, or the user can `! {cmd} pause`"
            )));
        }
    };
    let undo = deploy.undo.as_deref().unwrap_or("Deploy again what `{record}` printed: {before}");
    let effect = Effect {
        kind: "deploy".into(),
        at: now(),
        undo: render(&undo.replace("{record}", command), &before),
        record: command.into(),
        before,
        ..Effect::default()
    };
    st.effects.get_or_insert_with(Vec::new).push(effect);
    repo.save_state(&fid, &st)?;
    Ok(None)
}

/// The effects neither undone nor kept, numbered from 1 in the whole ledger.
fn open(st: &State) -> impl DoubleEndedIterator<Item = (usize, &Effect)> {
    let all = st.effects.as_deref().unwrap_or_default();
    all.iter().enumerate().map(|(i, e)| (i + 1, e)).filter(|(_, e)| e.open())
}

/// Why `doing` must wait: effects still to undo or keep.
pub fn open_problem(repo: &Repo, fid: &str, st: &State, doing: &str) -> Option<String> {
    let open: Vec<String> = open(st).map(|(n, e)| format!("{n} ({})", e.kind)).collect();
    if open.is_empty() {
        return None;
    }
    let cmd = flow_cmd(&repo.root);
    Some(format!(
        "{fid} has effects outside the repo that {doing} would leave behind: {}. Undo them \
         (`{cmd} effects --prompt` says how, `{cmd} effects check` confirms) or keep them \
         (`! {cmd} accept <n>`)",
        open.join(", ")
    ))
}

/// `flow effects [--prompt|check]`.
pub fn effects(repo: &Repo, args: &[String]) -> Result<i32> {
    let (fid, mut st) = repo.require_active()?;
    let cmd = flow_cmd(&repo.root);
    match args.first().map(String::as_str) {
        None => {
            let all = st.effects.as_deref().unwrap_or_default();
            if all.is_empty() {
                println!("no effects recorded");
            }
            for (i, e) in all.iter().enumerate() {
                let state = match (&e.undone, &e.accepted) {
                    (Some(at), _) => format!("undone {at}"),
                    (_, Some(at)) => format!("kept {at}"),
                    _ => "to undo".into(),
                };
                println!("{} {} {}  {state}\n  before: {}", i + 1, e.kind, e.at, e.before);
                if e.open() {
                    println!("  undo: {}", e.undo);
                }
            }
            Ok(0)
        }
        Some("--prompt") => {
            if open(&st).next().is_none() {
                println!("nothing to undo");
                return Ok(0);
            }
            println!("Undo what {fid} did outside the repo, newest first:\n");
            for (n, e) in open(&st).rev() {
                println!("{n}. The {} at {}:\n   {}\n", e.kind, e.at, e.undo);
                println!("   Undone when `{}` prints:\n   {}\n", e.record, e.before);
            }
            println!(
                "Then run `{cmd} effects check`: it runs each command and marks what is \
                 undone. If a step can't be done, stop and tell the user: they can keep an \
                 effect with `! {cmd} accept <n>`."
            );
            Ok(0)
        }
        Some("check") => {
            let mut left = 0;
            let effects = st.effects.as_deref_mut().unwrap_or_default();
            for (i, e) in effects.iter_mut().enumerate().rev().filter(|(_, e)| e.open()) {
                let now_printed = record(repo, &e.record)?;
                if same(&now_printed, &e.before) {
                    e.undone = Some(now());
                    println!("{} {}: undone", i + 1, e.kind);
                } else {
                    left += 1;
                    println!(
                        "{} {}: not undone; `{}` prints {now_printed}, not {}",
                        i + 1,
                        e.kind,
                        e.record,
                        e.before
                    );
                }
            }
            repo.save_state(&fid, &st)?;
            Ok(if left == 0 { 0 } else { 1 })
        }
        Some(_) => bail!("usage: flow effects [--prompt|check]"),
    }
}

/// `flow accept <n>`: the user keeps effect n as it is.
pub fn accept(repo: &Repo, args: &[String]) -> Result<i32> {
    let (fid, mut st) = repo.require_active()?;
    let n: Option<usize> = args.first().and_then(|a| a.parse().ok());
    let effects = st.effects.as_deref_mut().unwrap_or_default();
    let Some(effect) = n.and_then(|n| n.checked_sub(1)).and_then(|i| effects.get_mut(i)) else {
        bail!("usage: flow accept <n>, n an effect `flow effects` lists");
    };
    if !effect.open() {
        bail!("effect {} is already undone or kept", n.unwrap_or_default());
    }
    effect.accepted = Some(now());
    repo.save_state(&fid, &st)?;
    println!("{fid}: effect {} kept as it is", n.unwrap_or_default());
    Ok(0)
}
