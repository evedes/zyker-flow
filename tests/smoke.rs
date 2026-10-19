//! Smoke-tests flow against the real repos that use it, and that check itself.
//!
//! `real_repos` (ignored by default; `scripts/smoke` runs it) runs a flow binary in every
//! repo with a .flow.json in ~/Work, or in the repos given: `flow status`, the
//! session-start hook, and the pre-tool-use hook on a fixed set of tool calls. Each call's
//! expected verdict follows from that repo's own .flow.json and state. Nothing is
//! written: `status` and the hooks only read.
//!
//! $FLOW_SMOKE_BIN is the binary to run (default: this checkout's bin/flow), and
//! $FLOW_SMOKE_REPOS the repos, whitespace-separated (default: ~/Work/*/.flow.json's).

mod common;

use common::*;
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(120);

/// Runs `flow args` in `cwd` on `stdin`: its stdout, or why it failed.
fn run_flow(flow: &Path, args: &[&str], cwd: &Path, stdin: &str) -> Result<String, String> {
    let mut c = Command::new(flow);
    c.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = spawn(c.stderr(Stdio::piped()));
    // A command that exits without reading closes the pipe; that is not a failure here.
    let _ = child.stdin.take().expect("stdin").write_all(stdin.as_bytes());
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", TIMEOUT.as_secs()));
            }
            Err(e) => return Err(format!("wait: {e}")),
        }
    };
    let (out, err) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
    if !status.success() {
        let text = if err.trim().is_empty() { out.trim() } else { err.trim() };
        let tail = text.char_indices().rev().nth(399).map_or(text, |(i, _)| &text[i..]);
        let code = status.code().map_or("a signal".into(), |c| c.to_string());
        return Err(format!("exit {code}: {tail}"));
    }
    Ok(out)
}

/// The hook's `hookSpecificOutput` (None when it printed nothing), or why it failed.
fn hook(flow: &Path, root: &Path, event: &str, payload: Value) -> Result<Option<Value>, String> {
    let mut payload = payload;
    payload["cwd"] = json!(root);
    let out = run_flow(flow, &["hook", event], root, &payload.to_string())?;
    if out.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<Value>(&out) {
        Ok(v) if v["hookSpecificOutput"].is_object() => Ok(Some(v["hookSpecificOutput"].clone())),
        _ => Err(format!("not hook output: {}", out.trim().chars().take(200).collect::<String>())),
    }
}

/// A smoke case: label, tool, tool_input, whether the hook should deny it.
type Case = (String, &'static str, Value, bool);

fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The cases for this repo's config and state.
fn cases(root: &Path) -> Result<Vec<Case>, String> {
    let cfg = read_json(&root.join(".flow.json"))?;
    let work = cfg["work_dir"].as_str().unwrap_or("work");
    let global = root.join(work).join("flow.json");
    let global = if global.is_file() { read_json(&global)? } else { json!({}) };
    let phase = match global["active"].as_str().filter(|a| !a.is_empty()) {
        Some(active) => {
            let state = read_json(&root.join(work).join(active).join("state.json"))?;
            Some(state["phase"].as_str().ok_or("state.json has no phase")?.to_string())
        }
        None => None,
    };
    let paused = global["paused"].as_bool().unwrap_or(false);
    let code_open = paused || phase.as_deref() == Some("build");

    let path = |rel: &str| json!(root.join(rel));
    let mut out: Vec<Case> = vec![
        (
            "edit a file outside protected_paths".into(),
            "Edit",
            json!({"file_path": path("flow-smoke.txt")}),
            false,
        ),
        (
            "write a state file".into(),
            "Write",
            json!({"file_path": path(&format!("{work}/flow.json")), "content": "{}"}),
            true,
        ),
        ("run a read-only command".into(), "Bash", json!({"command": "git status"}), false),
        ("run a user-only command".into(), "Bash", json!({"command": "flow approve spec"}), true),
    ];
    if let Some(p) = cfg["protected_paths"].get(0).and_then(Value::as_str) {
        let code = if p.ends_with('/') || root.join(p).is_dir() {
            format!("{}/flow-smoke.txt", p.trim_end_matches('/'))
        } else {
            p.to_string()
        };
        out.push((format!("edit {code}"), "Edit", json!({"file_path": path(&code)}), !code_open));
        let redirect = json!({"command": format!("echo x > {code}")});
        out.push((format!("redirect into {code}"), "Bash", redirect, !code_open));
    }
    Ok(out)
}

/// Checks one repo: the number of failures, and the report's lines.
fn check_repo(flow: &Path, root: &Path) -> (usize, String) {
    let mut failures = 0;
    let name = root.file_name().map_or("".into(), |n| n.to_string_lossy());
    let mut text = format!("{name} ({})\n", root.display());
    let mut report = |ok: bool, label: &str, detail: &str| {
        failures += usize::from(!ok);
        let mark = if ok { "ok  " } else { "FAIL" };
        let detail = if detail.is_empty() { String::new() } else { format!(": {detail}") };
        let _ = writeln!(text, "  {mark}  {label}{detail}");
    };

    match run_flow(flow, &["status"], root, "") {
        Ok(out) => report(true, "flow status", out.trim().lines().next().unwrap_or("")),
        Err(e) => report(false, "flow status", &e),
    }

    match hook(flow, root, "session-start", json!({})) {
        Ok(res) => {
            let ctx = res.as_ref().and_then(|r| r["additionalContext"].as_str()).unwrap_or("");
            let detail = if ctx.is_empty() { "no context" } else { ctx };
            report(ctx.starts_with("flow:"), "session-start hook", detail);
        }
        Err(e) => report(false, "session-start hook", &e),
    }

    let checks = match cases(root) {
        Ok(checks) => checks,
        Err(e) => {
            report(false, "read .flow.json and state", &e);
            return (failures, text);
        }
    };
    for (label, tool, tool_input, should_deny) in checks {
        let payload = json!({"tool_name": tool, "tool_input": tool_input});
        let res = match hook(flow, root, "pre-tool-use", payload) {
            Ok(res) => res.unwrap_or_else(|| json!({})),
            Err(e) => {
                report(false, &label, &e);
                continue;
            }
        };
        let reason = res["permissionDecisionReason"].as_str().unwrap_or("");
        if reason.contains("hook error") {
            report(false, &label, reason);
            continue;
        }
        let denied = res["permissionDecision"] == "deny";
        let got = if denied { format!("denied ({reason})") } else { "allowed".into() };
        if denied == should_deny {
            report(true, &label, &got);
        } else {
            let want = if should_deny { "denied" } else { "allowed" };
            report(false, &label, &format!("{got}, expected {want}"));
        }
    }
    (failures, text)
}

/// Checks one repo, printing the report: the number of failures.
fn smoke(flow: &Path, root: &Path) -> usize {
    let (failures, text) = check_repo(flow, root);
    print!("{text}");
    failures
}

/// The directories of ~/Work/*/.flow.json.
fn work_repos() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(Path::new(&home).join("Work")) else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> =
        entries.flatten().map(|e| e.path()).filter(|p| p.join(".flow.json").is_file()).collect();
    roots.sort();
    roots
}

#[test]
#[ignore = "reads the real repos in ~/Work; scripts/smoke runs it"]
fn real_repos() {
    let flow = match std::env::var_os("FLOW_SMOKE_BIN") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => checkout("bin/flow"),
    };
    let given = std::env::var("FLOW_SMOKE_REPOS").unwrap_or_default();
    let mut roots: Vec<PathBuf> = given.split_whitespace().map(PathBuf::from).collect();
    if roots.is_empty() {
        roots = work_repos();
    }
    assert!(!roots.is_empty(), "no repos with a .flow.json in ~/Work");
    println!("flow: {}\n", flow.display());
    let mut failures = 0;
    for root in &roots {
        if !root.join(".flow.json").is_file() {
            println!("{}: no .flow.json\n", root.display());
            failures += 1;
            continue;
        }
        failures += smoke(&flow, root);
        println!();
    }
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    println!("{}, {}", plural(roots.len(), "repo"), plural(failures, "failure"));
    assert_eq!(failures, 0, "smoke failures");
}

#[test]
fn smoke_passes_whatever_the_state() {
    let t = Flow::new();
    let steps: [&dyn Fn(); 4] = [
        &|| {},
        &|| t.to_build(),
        &|| {
            t.flow(&["back"]);
        },
        &|| {
            t.flow(&["pause"]);
        },
    ];
    for step in steps {
        step();
        assert_eq!(smoke(&flow_bin(), &t.root), 0, "phase {}", t.phase());
    }
}

#[test]
fn smoke_catches_a_broken_hook() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    let stubs = TempDir::new();
    let crash = r#"echo '{"hookSpecificOutput": {"permissionDecision": "deny", "permissionDecisionReason": "flow: hook error: boom"}}'"#;
    for (name, body, expected) in [
        ("crashing", crash, "hook error: boom"),
        ("permissive", "true", "allowed, expected denied"),
    ] {
        let stub = stubs.path().join(name);
        let script = format!(
            "#!/bin/sh\nif [ \"$1 $2\" = \"hook pre-tool-use\" ]; then cat >/dev/null; {body}; exit 0; fi\n\
             exec \"{}\" \"$@\"\n",
            flow_bin().display()
        );
        write(&stub, script);
        chmod_x(&stub);
        let (failures, out) = check_repo(&stub, &t.root);
        assert!(failures > 0, "{out}");
        assert_in(expected, &out);
    }
}
