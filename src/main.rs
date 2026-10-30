//! flow — a phase-gated feature workflow for Claude Code.
//!
//! explore -> spec -> tickets -> build -> review -> ship -> done
//!
//! State lives in <work_dir>/flow.json (active feature, paused flag) and
//! <work_dir>/<NNN-slug>/state.json (phase, approvals, gate and review results).
//! Only this CLI writes them; the PreToolUse hook (`flow hook pre-tool-use`)
//! stops the agent from editing them, from running the user-only commands, and
//! from editing code, committing or pushing out of phase.
//!
//! Repos opt in with a `.flow.json` at the git root (`flow init` writes one);
//! everywhere else the hooks do nothing.

/// One of flow's own regexes (they compile), built on first use.
macro_rules! regex {
    ($pattern:expr) => {
        std::sync::LazyLock::new(|| regex::Regex::new($pattern).expect("built-in pattern"))
    };
}

mod checkpoint;
mod checks;
mod commands;
mod config;
mod deploy;
mod effects;
mod error;
mod hooks;
mod judge;
mod markdown;
mod paths;
mod primer;
mod repo;
mod shell;
mod specs;

use error::{Error, Result};
use std::env;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::process::ExitCode;

/// Whitespace as flow reads it: Unicode whitespace plus the separators \x1c-\x1f.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

const SUMMARY: &str = "flow — a phase-gated feature workflow for Claude Code.";

const COMMANDS: [&str; 18] = [
    "init", "new", "status", "check", "advance", "back", "approve", "delegate", "judge", "gates",
    "deploy", "effects", "accept", "pause", "resume", "rewind", "reset", "abandon",
];

/// Runs `f` with a panic turned into an error: a hook must fail closed, not crash.
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|p| {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        Err(Error(msg))
    })
}

fn hook(event: &str) {
    let stdin = io::read_to_string(io::stdin());
    let out = match event {
        "pre-tool-use" => match guarded(|| hooks::pre_tool_use_input(stdin)) {
            Ok(reason) => reason.map(|r| hooks::deny_output(&r)),
            Err(e) => Some(hooks::deny_output(&format!("hook error: {e}"))),
        },
        // session-start says nothing on errors.
        "session-start" => guarded(|| hooks::session_start_input(stdin)).unwrap_or_default(),
        _ => None,
    };
    if let Some(out) = out {
        let _ = writeln!(io::stdout(), "{out}");
    }
}

fn run(name: &str, args: &[String]) -> Result<i32> {
    let cwd = env::current_dir()?;
    if name == "init" {
        let root = repo::git_root(&cwd)?.ok_or("not inside a git repository")?;
        return commands::init(&root);
    }
    let Some(root) = repo::find_root(&cwd)? else {
        eprintln!(
            "flow: no .flow.json at the git root; this repo does not use flow (`flow init` opts it in)"
        );
        return Ok(2);
    };
    let repo = repo::Repo::open(&root)?;
    match name {
        "new" => commands::new(&repo, args),
        "status" => commands::status(&repo),
        "check" => commands::check_cmd(&repo),
        "advance" => commands::advance(&repo),
        "back" => commands::back(&repo),
        "approve" => commands::approve(&repo, args),
        "delegate" => commands::delegate(&repo, args),
        "judge" => commands::judge(&repo),
        "gates" => commands::gates(&repo),
        "deploy" => commands::deploy(&repo),
        "effects" => effects::effects(&repo, args),
        "accept" => effects::accept(&repo, args),
        "pause" => commands::pause(&repo, true),
        "resume" => commands::pause(&repo, false),
        "rewind" => checkpoint::rewind(&repo, args),
        "reset" => checkpoint::reset(&repo),
        "abandon" => commands::abandon(&repo),
        _ => unreachable!("only known commands are dispatched"),
    }
}

fn main() -> ExitCode {
    // Panics are reported like any other error, as `flow: ...` or in the hook's output.
    panic::set_hook(Box::new(|_| {}));
    let args: Vec<String> = match env::args_os().skip(1).map(|a| a.into_string()).collect() {
        Ok(args) => args,
        Err(arg) => {
            eprintln!("flow: argument {arg:?} is not valid UTF-8");
            return ExitCode::FAILURE;
        }
    };
    if let [first, event, ..] = args.as_slice() {
        if first == "hook" {
            hook(event);
            return ExitCode::SUCCESS;
        }
    }
    let Some(name) = args.first().filter(|a| COMMANDS.contains(&a.as_str())) else {
        println!("{SUMMARY}\n\ncommands: {}", COMMANDS.join(", "));
        return ExitCode::from(if args.is_empty() { 0 } else { 2 });
    };
    match guarded(|| run(name, &args[1..])) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("flow: {e}");
            ExitCode::FAILURE
        }
    }
}
