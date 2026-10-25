//! End-to-end tests for the flow CLI and its hooks, against throwaway git repos.
//!
//! The workflow and its hooks first, then flow's exact messages, exit codes, state files
//! and hashes, then the edge cases. Run the suite against another build (bin/flow, say)
//! with FLOW_TEST_BIN=<absolute path> cargo test --test flow.

mod common;

use common::*;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::sync::mpsc;

// --- the workflow and its hooks --------------------------------------------------

#[test]
fn new_scaffolds_research_and_blocks_code() {
    let t = Flow::new();
    t.flow(&["new", "hello", "Hello"]);
    assert!(t.fdir().join("research.md").is_file());
    assert_in("phase explore", &denied(t.edit("lib/app.ex")));
    assert_eq!(t.edit("README.md"), None);
    assert_in("still active", &t.flow_any(&["new", "other"]).stderr);
}

#[test]
fn placeholders_and_open_questions_block_advance() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    assert_in("TODO(flow)", &t.flow_any(&["advance"]).stderr);
    t.fwrite("research.md", &RESEARCH.replace("[x]", "[ ]"));
    assert_in("unanswered", &t.flow_any(&["advance"]).stderr);
}

#[test]
fn spec_needs_valid_approval() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    assert!(t.fdir().join("spec.md").is_file());
    assert_in("not complete", &t.flow_any(&["approve", "spec"]).stderr);
    t.fwrite("spec.md", SPEC);
    assert_in("approve spec", &t.flow_any(&["advance"]).stderr);
    t.flow(&["approve", "spec"]);
    t.fwrite("spec.md", &format!("{SPEC}\nchanged\n"));
    assert_in("approve spec", &t.flow_any(&["advance"]).stderr);
    t.flow(&["approve", "spec"]);
    t.flow(&["advance"]);
    assert_eq!(t.phase(), "tickets");
}

#[test]
fn default_templates_decide_the_required_sections() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    t.fwrite("spec.md", &SPEC.replace("## UI\nNone.\n", ""));
    assert_in("'## ui' is missing", &t.flow_any(&["check"]).stdout);
    t.fwrite("spec.md", &SPEC.replace("## Spec changes\nNone.\n", ""));
    assert_in("'## spec changes' is missing", &t.flow_any(&["check"]).stdout);
    t.fwrite("spec.md", &SPEC.replace("## Open questions\nNone.\n", ""));
    // only the approval is missing
    assert_in("approve spec", &t.flow_any(&["check"]).stdout);
}

#[test]
fn a_heading_in_a_code_fence_is_text() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    // The fence holds a `~~~` line, which doesn't close it, and a `## UI` that isn't one.
    let fenced = "Shows hello:\n  ```md\n~~~\n## UI\nNone.\n  ```\n";
    t.fwrite("spec.md", &SPEC.replace("## UI\nNone.\n", "").replace("Shows hello.\n", fenced));
    assert_in("'## ui' is missing", &t.flow_any(&["check"]).stdout);
}

#[test]
fn repo_templates_override_the_plugin_ones() {
    let t = Flow::new();
    t.write(
        "work/templates/spec.md",
        "# Spec: {title}\n\n## Goal\nTODO(flow): goal\n\n\
         ## Acceptance criteria\nTODO(flow): checks\n\n## Notes\n",
    );
    t.flow(&["new", "hello", "Hello"]);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    assert_in("## Notes", &t.fread("spec.md"));
    t.fwrite("spec.md", "# Spec\n## Acceptance criteria\nHello is shown.\n");
    assert_in("'## goal' is missing", &t.flow_any(&["check"]).stdout);
    t.fwrite("spec.md", "# Spec\n## Goal\nHi.\n## Acceptance criteria\nHello is shown.\n");
    t.flow(&["approve", "spec"]); // no data model or UI section needed
}

#[test]
fn tickets_need_verification_commands() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    t.fwrite("spec.md", SPEC);
    t.flow(&["approve", "spec"]);
    t.flow(&["advance"]);
    assert_in("from `work/STANDING.md`", &t.fread("tickets/T01.md"));
    t.fwrite("tickets/T01.md", &TICKET.replace("test -f lib/app.ex", ""));
    assert_in("no commands", &t.flow_any(&["check"]).stdout);
}

#[test]
fn build_allows_code_and_gates_go_stale() {
    let t = Flow::new();
    t.to_build();
    assert_eq!(t.edit("lib/app.ex"), None);
    assert_in("gates", &t.flow_any(&["advance"]).stderr);
    assert_ne!(t.flow_any(&["gates"]).code, 0); // lib/app.ex does not exist yet
    t.write("lib/app.ex", "hello\n");
    t.flow(&["gates"]);
    t.write("lib/app.ex", "changed\n");
    assert_in("gates", &t.flow_any(&["advance"]).stderr);
}

#[test]
fn state_files_and_user_commands_are_protected() {
    let t = Flow::new();
    t.to_build();
    assert_in("state files", &denied(t.edit("work/001-hello/state.json")));
    assert_in("state files", &denied(t.edit("work/flow.json")));
    let echo = t.bash("echo {} > work/001-hello/state.json");
    assert_in("state files", &denied(echo));
    assert_eq!(t.bash("cat work/001-hello/state.json"), None);
    for c in ["flow approve spec", "plugins/flow/bin/flow pause", "flow abandon"] {
        assert_in("user's", &denied(t.bash(c)));
    }
    assert_eq!(t.bash("flow status"), None);
}

#[test]
fn work_dir_defaults_to_flow() {
    let t = Flow::new();
    let mut cfg = parse(&t.read(".flow.json"));
    cfg.as_object_mut().unwrap().remove("work_dir");
    t.write(".flow.json", cfg.to_string());
    t.flow(&["new", "hello"]);
    assert!(t.path("flow/001-hello/state.json").is_file());
    assert_in("state files", &denied(t.edit("flow/flow.json")));
    assert_in("state files", &denied(t.bash("rm flow/001-hello/state.json")));
    assert_in("rules", &denied(t.bash("echo x >> flow/STANDING.md")));
    // The binary shares the directory's name; running or copying it names no state.
    for c in ["flow status", "cp bin/flow /tmp/flow && flow status", "rm -rf /tmp/flow"] {
        assert_eq!(t.bash(c), None, "{c}");
    }
}

#[test]
fn bash_writes_to_code_follow_the_phase() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    for c in ["echo x > lib/app.ex", "sed -i s/a/b/ mix.exs", "mix phx.gen.live Blog Post posts"] {
        assert_in("phase explore", &denied(t.bash(c)));
    }
    assert_eq!(t.bash("grep -r hello lib/ 2>/dev/null"), None);
    assert_eq!(t.bash("ls lib/ >/dev/null 2>&1"), None);
    // Monitor runs its command in the same shell as Bash.
    let monitor = t.hook("Monitor", json!({"command": "echo x > lib/app.ex; echo done"}));
    assert_in("phase explore", &denied(monitor));
    assert_eq!(t.hook("Monitor", json!({"command": "tail -f lib/app.ex"})), None);
    assert_eq!(t.hook("Monitor", json!({"ws": {"url": "wss://example.com"}})), None);
}

#[test]
fn protected_paths_are_normalized_or_refused() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    t.configure(json!({"protected_paths": ["./lib/", "src//./core/"]}));
    for rel in ["lib/app.ex", "src/core/x.rs"] {
        assert_in("phase explore", &denied(t.edit(rel)));
        assert_in("phase explore", &denied(t.bash(&format!("echo x > {rel}"))));
    }
    assert_eq!(t.edit("src/other.rs"), None);
    for (p, why) in [
        ("/lib/", "is absolute"),
        ("../lib", "leaves the repo"),
        ("lib/**", "is a glob"),
        ("./", "names the whole repo"),
        ("", "names the whole repo"),
    ] {
        t.configure(json!({"protected_paths": ["lib/", p]}));
        let error = format!(".flow.json protected path '{p}': {why}");
        assert_in(&format!("hook error: {error}"), &denied(t.edit("README.md")));
        assert_in(&error, &t.flow_any(&["status"]).stderr);
    }
}

#[test]
fn bash_hook_false_positives_are_allowed() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    let allowed = [
        // Text written to a file outside the repo's code, via a cat heredoc.
        "cat > /tmp/msg.txt <<'EOF'\nfix lib/app.ex and work/001-hello/state.json\nEOF",
        "cd /x && cat <<EOF > /tmp/t.md\nsee mix.exs\nEOF\ngit status",
        // A write and a read of code in separate commands.
        "rm work/001-hello/notes.md && grep -n x lib/app.ex",
        "grep -rn hello lib > /tmp/hits.txt",
        // Appending a note that mentions a state file somewhere else.
        "echo \"- note on work/001-hello/state.json\" >> /tmp/MEMORY.md",
        // Read-only format check.
        "mix format --check-formatted",
        "mix compile --warnings-as-errors && mix format --check-formatted && mix test",
        // Prose that mentions the user-only subcommands.
        "git log --grep 'flow: the user runs approve'",
    ];
    for c in allowed {
        assert_eq!(t.bash(c), None, "{c}");
    }
}

#[test]
fn bash_hook_still_catches_real_writes() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    let denied_cmds = [
        ("sed -i 's/a;b/c/' lib/app.ex", "phase explore"),
        ("cat > lib/app.ex <<'EOF'\nx\nEOF", "phase explore"),
        ("cd /x && echo y > lib/app.ex", "phase explore"),
        ("cp /tmp/a lib/app.ex", "phase explore"),
        ("bash -c 'rm -rf lib'", "phase explore"),
        ("mix format", "phase explore"),
        ("rm work/flow.json", "state files"),
        ("cat > work/001-hello/state.json <<EOF\n{}\nEOF", "state files"),
        ("bash -c 'flow approve spec'", "user's"),
        ("FOO=1 flow resume", "user's"),
    ];
    for (c, reason) in denied_cmds {
        let got = t.bash(c).unwrap_or_default();
        assert!(got.contains(reason), "{c}: {got:?} lacks {reason:?}");
    }
}

#[test]
fn codegen_patterns_come_from_config() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    t.configure(json!({"codegen": []}));
    assert_eq!(t.bash("mix phx.gen.live Blog Post posts"), None);
    t.configure(json!({"codegen": [r"\bnpx\s+prisma\s+generate\b"]}));
    assert_in("phase explore", &denied(t.bash("npx prisma generate")));
    t.configure(json!({"codegen": ["mix (phx"]}));
    assert_in("codegen pattern 'mix (phx'", &denied(t.edit("README.md")));
}

#[test]
fn command_provider_gates_push() {
    let t = Flow::new();
    t.configure(json!({"deploy": {"provider": "command", "status_command": "cat runs.json"}}));
    let runs = |status: &str| json!([{"name": "deploy", "status": status, "head_sha": "abc1234"}]);
    t.write("runs.json", runs("running").to_string());
    assert_in("deploy=running", &denied(t.bash("git push")));
    assert_in("deploy=running", &denied(t.bash("git -P push origin main")));
    t.write("runs.json", runs("success").to_string());
    assert_eq!(t.bash("git push"), None);
    t.write("runs.json", r#"{"not": "a list"}"#);
    assert_in("expected a list", &denied(t.bash("git push")));
    assert_in("expected a list", &t.flow_any(&["deploy"]).stderr);
}

#[test]
fn github_provider_reads_gh_run_list() {
    let mut t = Flow::new();
    let bindir = t.path(".bin");
    let gh = bindir.join("gh");
    write(&gh, "#!/bin/sh\necho \"$@\" > \"$0.args\"\ncat \"$0.json\"\n");
    chmod_x(&gh);
    t.set_env("PATH", path_with(&bindir));
    t.configure(json!({"deploy": {"provider": "github", "repo": "edo/x"}}));
    let mut run = json!({
        "databaseId": 7, "name": "CI", "status": "in_progress", "conclusion": "",
        "headSha": "abc1234",
    });
    write(&bindir.join("gh.json"), json!([run]).to_string());
    assert_in("CI=in_progress", &denied(t.bash("git push")));
    assert_in("--repo edo/x", &read(&bindir.join("gh.args")));
    run["status"] = json!("completed");
    run["conclusion"] = json!("failure");
    write(&bindir.join("gh.json"), json!([run]).to_string());
    assert_eq!(t.bash("git push"), None);
    assert_in("failure", &t.flow(&["deploy"]).stdout);
}

#[test]
fn deploy_prints_any_run_id_as_text() {
    let t = Flow::new();
    t.configure(json!({"deploy": {"provider": "command", "status_command": "cat runs.json"}}));
    let runs = json!([
        {"id": null, "name": "a", "status": "success", "head_sha": "abcdef12"},
        {"id": true, "name": "b", "status": "running", "head_sha": "x"},
        {"name": "c", "status": "x", "head_sha": "y"},
    ]);
    t.write("runs.json", runs.to_string());
    let out = t.flow(&["deploy"]).stdout;
    assert_in("  None a              success   abcdef1\n", &out);
    assert_in("  True b              running   x\n", &out);
    assert_in("   c              x         y\n", &out);
}

#[test]
fn a_gh_run_without_a_conclusion_is_none() {
    let mut t = Flow::new();
    let bindir = t.path(".bin");
    write(&bindir.join("gh"), "#!/bin/sh\ncat \"$0.json\"\n");
    chmod_x(&bindir.join("gh"));
    t.set_env("PATH", path_with(&bindir));
    t.configure(json!({"deploy": {"provider": "github"}}));
    let run = json!({"databaseId": 7, "name": "CI", "status": "completed", "conclusion": null,
                     "headSha": "abc1234"});
    write(&bindir.join("gh.json"), json!([run]).to_string());
    assert_in(" 7 CI             None      abc1234\n", &t.flow(&["deploy"]).stdout);
}

#[test]
fn unknown_provider_is_reported() {
    let t = Flow::new();
    t.configure(json!({"deploy": {"provider": "jenkins"}}));
    assert_in("deploy.provider", &denied(t.bash("git push")));
    t.configure(json!({"deploy": {"provider": "command"}}));
    assert_in("status_command", &denied(t.bash("git push")));
    // forgejo, from its keys
    t.configure(json!({"deploy": {
        "forgejo_url": "http://127.0.0.1:9", "repo": "edo/x", "token_env": "NO_SUCH_TOKEN",
    }}));
    assert_in("$NO_SUCH_TOKEN", &denied(t.bash("git push")));
}

#[test]
fn init_opts_a_repo_in() {
    let t = Flow::new();
    fs::remove_file(t.path(".flow.json")).unwrap();
    t.write("mix.exs", "defmodule X.MixProject do end\n");
    t.write(
        ".claude/settings.json",
        r#"{"permissions": {"deny": ["Bash(rm -rf:*)", "Bash(flow approve:*)"]}}"#,
    );
    assert_in("defaults for mix.exs", &t.flow(&["init"]).stdout);
    let cfg = parse(&t.read(".flow.json"));
    assert!(strings(&cfg["protected_paths"]).contains(&"lib/"));
    assert!(strings(&cfg["standing_gates"]).contains(&"mix test"));
    let name = t.root.file_name().unwrap().to_str().unwrap();
    assert_in(name, &t.read("flow/STANDING.md"));
    let settings = parse(&t.read(".claude/settings.json"));
    let deny = strings(&settings["permissions"]["deny"]);
    let approve_rules = deny.iter().filter(|r| **r == "Bash(flow approve:*)");
    assert_eq!(approve_rules.count(), 1);
    for rule in ["Bash(rm -rf:*)", "Bash(flow abandon:*)", "Edit(flow/*/state.json)"] {
        assert!(deny.contains(&rule), "{rule} not in {deny:?}");
    }
    assert!(!deny.iter().any(|r| r.starts_with("Write(")), "{deny:?}");
    assert_in("already exists", &t.flow_any(&["init"]).stderr);
    t.flow(&["new", "hello"]);
    let r = t.bash("mix phx.gen.live Blog Post posts");
    assert_in("phase explore", &denied(r));
}

#[test]
fn commit_gate() {
    let t = Flow::new();
    t.write("docs.md", "docs\n");
    assert_eq!(t.bash("git commit -am docs"), None);
    t.write("lib/x.ex", "x\n");
    assert_in("no active feature", &denied(t.bash("git commit -am x")));
    fs::remove_file(t.path("lib/x.ex")).unwrap();

    t.to_ship();
    assert_eq!(t.bash("git add -A && git commit -m hello"), None);
    t.write("lib/app.ex", "sneaky\n");
    assert_in("gates", &denied(t.bash("git commit -am hello")));
}

#[test]
fn review_fail_blocks_and_back_returns_to_build() {
    let t = Flow::new();
    t.to_build();
    t.write("lib/app.ex", "hello\n");
    t.flow(&["gates"]);
    t.flow(&["advance"]);
    assert!(t.fdir().join("review.md").is_file());
    t.fwrite("review.md", &REVIEW.replace("pass —", "fail —"));
    assert_in("Verdict", &t.flow_any(&["advance"]).stderr);
    t.flow(&["back"]);
    assert_eq!(t.phase(), "build");
}

#[test]
fn ship_to_done_after_commit() {
    let t = Flow::new();
    t.to_ship();
    assert_in("uncommitted", &t.flow_any(&["advance"]).stderr);
    t.git(&["add", "-A"]);
    t.git(&["commit", "-qm", "hello"]);
    t.flow(&["advance"]);
    assert_eq!(t.phase(), "done");
    t.flow(&["new", "next"]); // active slot is free again
}

#[test]
fn ship_needs_the_feature_folder_committed() {
    let t = Flow::new();
    t.to_ship();
    t.git(&["add", "lib"]);
    t.git(&["commit", "-qm", "code only"]);
    assert_in("work/001-hello/", &t.flow_any(&["advance"]).stderr);
    t.git(&["add", "work"]);
    t.git(&["commit", "-qm", "record"]);
    t.flow(&["advance"]);
    assert_eq!(t.phase(), "done");
}

#[test]
fn gates_run_only_in_build() {
    let t = Flow::new();
    t.to_build();
    t.write("lib/app.ex", "hello\n");
    t.flow(&["gates"]);
    t.flow(&["advance"]);
    t.fwrite("review.md", REVIEW);
    t.write("README.md", "changed after the review\n");
    assert_in("gates run in build", &t.flow_any(&["gates"]).stderr);
    assert_in("code changed since the gates passed", &t.flow_any(&["advance"]).stderr);
}

#[test]
fn code_changed_after_ship_needs_a_new_review() {
    let t = Flow::new();
    t.to_ship();
    t.flow(&["back"]);
    t.flow(&["back"]);
    t.write("lib/app.ex", "never reviewed\n");
    t.flow(&["gates"]);
    t.flow(&["advance"]);
    assert_in("pass", &t.fread("review-1.md")); // the old round, kept
    assert_in("TODO(flow)", &t.flow_any(&["advance"]).stderr);
    t.fwrite("review.md", REVIEW);
    t.flow(&["advance"]);
    assert_eq!(t.phase(), "ship");
}

#[test]
fn back_to_spec_keeps_the_tickets() {
    let t = Flow::new();
    t.to_build();
    let tickets = t.fdir().join("tickets");
    fs::rename(tickets.join("T01.md"), tickets.join("T01-hello.md")).unwrap();
    t.flow(&["back"]);
    t.flow(&["back"]);
    t.flow(&["advance"]);
    let names: Vec<String> = fs::read_dir(&tickets)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["T01-hello.md"]);
}

/// [`SPEC`] with `changes` as its `## Spec changes`.
fn spec_changing(changes: &str) -> String {
    SPEC.replace("## Spec changes\nNone.\n", &format!("## Spec changes\n{changes}\n"))
}

#[test]
fn spec_changes_merge_into_the_living_spec_at_done() {
    let t = Flow::new();
    t.write(
        "work/specs/greeting.md",
        "# Greeting\n\nWhat the app says.\n\n## Wave\nIt waves.\n\n## Hello\nSays hi.\n\n\
         ## Shout\nIt shouts.\n",
    );
    t.git(&["add", "-A"]);
    t.git(&["commit", "-qm", "living spec"]);
    let changes = "### Changed greeting: hello\nSays hello.\n\n```md\n## not a heading\n```\n\n\
                   ### Removed greeting: Shout\nToo loud.\n\n\
                   ### Added farewell: Goodbye\nSays goodbye.\n#### Scenario: leaving\nBye.\n";
    t.to_ship_with(&spec_changing(changes));
    t.git(&["add", "-A"]);
    t.git(&["commit", "-qm", "hello"]);
    assert_in("It shouts.", &t.read("work/specs/greeting.md")); // not before done
    assert!(!t.path("work/specs/farewell.md").exists());
    let r = t.flow(&["advance"]);
    assert_in(
        "merged the spec changes into work/specs/farewell.md, work/specs/greeting.md",
        &r.stdout,
    );
    assert_eq!(
        t.read("work/specs/greeting.md"),
        "# Greeting\n\nWhat the app says.\n\n## Wave\n\nIt waves.\n\n## hello\n\nSays hello.\n\n\
         ```md\n## not a heading\n```\n"
    );
    assert_eq!(
        t.read("work/specs/farewell.md"),
        "# farewell\n\n## Goodbye\n\nSays goodbye.\n#### Scenario: leaving\nBye.\n"
    );
}

#[test]
fn spec_changes_must_apply_to_the_living_spec() {
    let t = Flow::new();
    t.write(
        "work/specs/greeting.md",
        "# greeting\n## Hello\nSays hi.\n## Twice\nA.\n## twice\nB.\n",
    );
    t.flow(&["new", "hello"]);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    let check = |changes: &str| {
        t.fwrite("spec.md", &spec_changing(changes));
        t.flow_any(&["check"]).stdout
    };
    let has_no = "'### Changed greeting: Wave': work/specs/greeting.md has no requirement 'Wave'";
    assert_in(has_no, &check("### Changed greeting: Wave\nWaves."));
    assert_in("has no requirement 'Hi'", &check("### Removed other: Hi"));
    assert_in("work/specs/greeting.md already has it", &check("### Added greeting: HELLO\nHi."));
    assert_in("has it more than once", &check("### Removed greeting: Twice"));
    assert_in("text is missing", &check("### Added greeting: Wave"));
    assert_in("more than one entry", &check("### Added a: X\nx\n### Removed a: x"));
    assert_in("not `Added|Changed|Removed", &check("### Renamed greeting: Hello"));
    assert_in("'Greeting' is not an area name", &check("### Added Greeting: Wave\nWaves."));
    assert_in("no requirement named", &check("### Added greeting:\nWaves."));
    assert_in("has no `### Added|Changed|Removed", &check("We change the greeting."));
    assert_in("'## spec changes' is missing", &check("").replace("\n", " "));
    t.fwrite("spec.md", &spec_changing("### Changed greeting: Wave\nWaves."));
    assert_in("not complete", &t.flow_any(&["approve", "spec"]).stderr);
    // Only the approval is missing once they apply.
    assert_eq!(check("none"), check("### Changed greeting: hello\nSays hello."));
    assert_in("approve spec", &check("None."));
}

#[test]
fn spec_changes_are_checked_again_before_done() {
    let t = Flow::new();
    t.write("work/specs/greeting.md", "# greeting\n## Hello\nSays hi.\n");
    t.to_ship_with(&spec_changing("### Removed greeting: Hello"));
    t.write("work/specs/greeting.md", "# greeting\n");
    t.git(&["add", "-A"]);
    t.git(&["commit", "-qm", "hello"]);
    let r = t.flow_any(&["advance"]);
    assert_in("work/specs/greeting.md has no requirement 'Hello'", &r.stderr);
    assert_eq!(t.phase(), "ship");
}

#[test]
fn rules_change_only_by_hand_while_a_feature_is_active() {
    let t = Flow::new();
    t.write("work/specs/greeting.md", "# greeting\n");
    let rules = [
        ".flow.json",
        ".claude/settings.json",
        ".claude/settings.local.json",
        "work/STANDING.md",
        "work/templates/spec.md",
        "work/specs/x.md",
    ];
    for rule in rules {
        assert_eq!(t.edit(rule), None, "{rule} with no feature active");
    }
    t.flow(&["new", "hello"]);
    for rule in rules {
        assert_in("change only by hand", &denied(t.edit(rule)));
    }
    assert_eq!(t.edit(".claude/agents/x.md"), None);
    assert_in("change only by hand", &denied(t.bash("echo '{}' > .flow.json")));
    let hooks_off = "echo '{\"disableAllHooks\": true}' > .claude/settings.local.json";
    assert_in("change only by hand", &denied(t.bash(hooks_off)));
    assert_in("change only by hand", &denied(t.bash("rm .claude/settings.json")));
    assert_in("change only by hand", &denied(t.bash("rm -r work/specs")));
    assert_in("change only by hand", &denied(t.bash("sed -i s/a/b/ work/STANDING.md")));
    assert_eq!(t.bash("cat .flow.json work/specs/greeting.md"), None);
    assert_eq!(t.edit("work/001-hello/research.md"), None);
    t.flow(&["pause"]);
    assert_in("change only by hand", &denied(t.edit(".flow.json")));
}

/// A `claude` on PATH that records its arguments and prompt and replies with `verdict`.
struct Judge {
    dir: TempDir,
}

impl Judge {
    fn new(t: &mut Flow) -> Judge {
        let dir = TempDir::new();
        let claude = dir.path().join("claude");
        write(
            &claude,
            "#!/bin/sh\necho \"$@\" > \"$0.args\"\ncat > \"$0.prompt\"\ncat \"$0.json\"\n",
        );
        chmod_x(&claude);
        t.set_env("PATH", path_with(dir.path()));
        let judge = Judge { dir };
        judge.says("approve", "Looks right.", &[]);
        judge
    }

    fn says(&self, verdict: &str, summary: &str, findings: &[&str]) {
        let output = json!({
            "verdict": verdict, "summary": summary, "findings": findings,
            "questions": if verdict == "ask-user" { json!(["Blue or green?"]) } else { json!([]) },
        });
        let reply = json!({"type": "result", "subtype": "success", "is_error": false,
                           "result": output.to_string(), "structured_output": output});
        write(&self.dir.path().join("claude.json"), reply.to_string());
    }

    fn replies(&self, reply: &str) {
        write(&self.dir.path().join("claude.json"), reply);
    }

    fn file(&self, ext: &str) -> String {
        let path = self.dir.path().join(format!("claude.{ext}"));
        let text = if path.exists() { read(&path) } else { String::new() };
        let _ = fs::remove_file(path);
        text
    }
}

/// Into phase spec, the spec complete but not approved.
fn at_spec(t: &Flow) {
    t.flow(&["new", "hello", "Hello"]);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    t.fwrite("spec.md", SPEC);
}

#[test]
fn the_judge_passes_only_what_the_user_delegated() {
    let mut t = Flow::new();
    let judge = Judge::new(&mut t);
    at_spec(&t);
    assert_in("has not delegated spec", &t.flow_any(&["judge"]).stderr);
    assert_eq!(judge.file("args"), ""); // never ran
    assert_in("are the user's", &denied(t.bash("flow delegate spec")));
    assert_in("usage: flow delegate", &t.flow_any(&["delegate", "build"]).stderr);
    t.flow(&["delegate", "spec"]);
    assert_in("or the judge's: `", &t.flow_any(&["check"]).stdout);
    assert_in("delegated to the judge: spec", &t.flow(&["status"]).stdout);
    let r = t.flow(&["judge"]);
    assert_in("judge: approve — Looks right.", &r.stdout);
    let args = judge.file("args");
    assert_in("-p --output-format json --no-session-persistence", &args);
    assert_in("--tools Read,Grep,Glob --allowedTools Read,Grep,Glob --json-schema", &args);
    let prompt = judge.file("prompt");
    assert_in("delegated to you their approval of its spec", &prompt);
    assert_in("<artifact path=\"work/001-hello/spec.md\">\n# Spec: x", &prompt);
    assert_in("<artifact path=\"work/001-hello/research.md\">", &prompt);
    assert_in("Judge the spec (spec.md)", &prompt);
    assert!(!prompt.contains("Judge the tickets"), "{prompt}");
    assert_eq!(t.state()["approvals"]["spec"]["by"], "judge");
    assert_in("approval spec: valid (by the judge)", &t.flow(&["status"]).stdout);
    t.flow(&["advance"]);
    // Tickets weren't delegated, and an edit voids the judge's approval like the user's.
    t.fwrite("tickets/T01.md", TICKET);
    assert_in("has not delegated tickets", &t.flow_any(&["judge"]).stderr);
    t.flow(&["back"]);
    t.fwrite("spec.md", &format!("{SPEC}\nchanged\n"));
    assert_in("approve spec", &t.flow_any(&["check"]).stdout);
    t.flow(&["delegate", "off"]);
    assert_in("has not delegated spec", &t.flow_any(&["judge"]).stderr);
}

#[test]
fn a_rejection_is_kept_per_version_and_hands_back_after_three() {
    let mut t = Flow::new();
    let judge = Judge::new(&mut t);
    at_spec(&t);
    t.flow(&["delegate", "spec"]);
    judge.says("reject", "Thin.", &["spec.md Behaviour: no error states"]);
    let r = t.flow_any(&["judge"]);
    assert_eq!(r.code, 1);
    assert_in("judge: reject — Thin.\n  - spec.md Behaviour: no error states", &r.stdout);
    assert!(t.state()["approvals"]["spec"].is_null());
    judge.file("args");
    // The same version again: the same verdict, without asking the judge.
    judge.says("approve", "Fine now.", &[]);
    assert_in("judged this version of spec already", &t.flow_any(&["judge"]).stdout);
    assert_eq!(judge.file("args"), "");
    for n in 2..=3 {
        judge.says("reject", "Still thin.", &[]);
        t.fwrite("spec.md", &format!("{SPEC}\nround {n}\n"));
        assert_eq!(t.flow_any(&["judge"]).code, 1);
    }
    t.fwrite("spec.md", SPEC.replace("Shows hello.", "Shows hello, or an error.").as_str());
    let r = t.flow_any(&["judge"]);
    assert_in("rejected spec 3 times since it was delegated", &r.stderr);
    t.flow(&["delegate", "spec"]); // three more tries
    judge.says("approve", "Fine now.", &[]);
    t.flow(&["judge"]);
    assert_eq!(t.state()["judgements"].as_array().unwrap().len(), 4);
}

#[test]
fn the_judge_asks_the_user_and_fails_closed() {
    let mut t = Flow::new();
    let judge = Judge::new(&mut t);
    at_spec(&t);
    t.flow(&["delegate", "spec"]);
    judge.says("ask-user", "A product call.", &[]);
    let r = t.flow_any(&["judge"]);
    assert_eq!(r.code, 1);
    assert_in("questions for the user:\n  - Blue or green?", &r.stdout);
    t.fwrite("spec.md", &format!("{SPEC}\nv2\n"));
    let failed =
        r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"result":"over"}"#;
    judge.replies(failed);
    assert_in("the judge failed (error_max_budget_usd)", &t.flow_any(&["judge"]).stderr);
    judge.replies("not json");
    assert_in("the judge's reply is not JSON", &t.flow_any(&["judge"]).stderr);
    judge.replies(r#"{"is_error":false,"structured_output":{"verdict":"yes","summary":""}}"#);
    assert_in("\"yes\" is not one of approve, reject, ask-user", &t.flow_any(&["judge"]).stderr);
    assert_eq!(t.state()["judgements"].as_array().unwrap().len(), 1);
    assert!(t.state()["approvals"]["spec"].is_null());
    t.fwrite("spec.md", &SPEC.replace("Goodbyes.", "TODO(flow): later"));
    assert_in("spec is not ready to judge", &t.flow_any(&["judge"]).stderr);
}

#[test]
fn a_delegated_review_needs_the_judge_not_review_md() {
    let mut t = Flow::new();
    let judge = Judge::new(&mut t);
    t.write("work/STANDING.md", "# Standing\n");
    t.to_build();
    t.write("lib/app.ex", "hello\n");
    t.flow(&["gates"]);
    t.flow(&["delegate", "review"]);
    t.flow(&["advance"]);
    t.fwrite("review.md", REVIEW); // the agent's own pass doesn't count
    assert_in("review needs the user's approval", &t.flow_any(&["advance"]).stderr);
    judge.says("reject", "No test for hello.", &["lib/app.ex:1 untested"]);
    assert_eq!(t.flow_any(&["judge"]).code, 1);
    assert_in("## Verdict\nfail — No test for hello.", &t.fread("review.md"));
    let prompt = judge.file("prompt");
    assert_in("[standing] true => exit 0", &prompt);
    assert_in("<artifact path=\"git diff HEAD\">", &prompt);
    assert_in("lib/app.ex", &prompt); // a new file
    assert_in("<rules path=\"work/STANDING.md\">", &prompt);
    assert_in("<artifact path=\"work/001-hello/tickets/T01.md\">", &prompt);
    t.flow(&["back"]);
    t.write("lib/app_test.ex", "test\n");
    t.flow(&["gates"]);
    t.flow(&["advance"]);
    judge.says("approve", "Tested now.", &[]);
    t.flow(&["judge"]);
    assert_in("## Verdict\npass — Tested now.", &t.fread("review.md"));
    t.flow(&["advance"]);
    assert_eq!(t.phase(), "ship");
}

#[test]
fn the_user_can_approve_the_review() {
    let t = Flow::new();
    t.to_build();
    t.write("lib/app.ex", "hello\n");
    t.flow(&["gates"]);
    assert_in(
        "review can only be approved in phase review",
        &t.flow_any(&["approve", "review"]).stderr,
    );
    t.flow(&["advance"]);
    assert_in("review.md", &t.flow_any(&["check"]).stdout); // review.md not filled in
    t.flow(&["approve", "review"]);
    t.flow(&["check"]);
    t.flow(&["back"]);
    t.write("lib/app.ex", "changed\n");
    t.flow(&["gates"]);
    t.flow(&["advance"]);
    assert_in("review.md", &t.flow_any(&["check"]).stdout); // that approval was of other code
}

#[test]
fn approved_artifacts_changed_later_block_gates_and_advance() {
    let t = Flow::new();
    t.to_build();
    t.fwrite("tickets/T01.md", &TICKET.replace("test -f lib/app.ex", "true"));
    assert_in("tickets changed since it was approved", &t.flow_any(&["gates"]).stderr);
    t.fwrite("tickets/T01.md", TICKET);
    t.fwrite("spec.md", &format!("{SPEC}\nchanged\n"));
    assert_in("spec changed since it was approved", &t.flow_any(&["check"]).stdout);
    t.flow(&["back"]);
    assert_in("spec changed", &t.flow_any(&["approve", "tickets"]).stderr);
}

#[test]
fn approve_checks_sections_named_approval() {
    let t = Flow::new();
    t.write(
        "work/templates/spec.md",
        "## Goal\nTODO(flow): goal\n\n## Approval notes\nTODO(flow): who\n",
    );
    t.flow(&["new", "hello"]);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    t.fwrite("spec.md", "## Goal\nHi.\n");
    assert_in("'## approval notes' is missing", &t.flow_any(&["approve", "spec"]).stderr);
}

#[test]
fn missing_state_of_the_active_feature_is_reported() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    t.git(&["add", "work/flow.json"]);
    t.git(&["commit", "-qm", "track flow.json"]);
    t.git(&["clean", "-fdq", "work/"]);
    assert_in("has no work/001-hello/state.json", &t.flow_any(&["status"]).stderr);
    assert_in("abandon", &denied(t.edit("lib/app.ex")));
    let r = t.hook_raw("session-start", json!({"cwd": t.root}).to_string());
    let ctx = &parse(&r.stdout)["hookSpecificOutput"]["additionalContext"];
    assert_in("has no work/001-hello/state.json", ctx.as_str().unwrap());
    t.flow(&["abandon"]);
    t.flow(&["new", "next"]);
}

#[test]
fn new_numbers_the_slug_itself() {
    let t = Flow::new();
    t.flow(&["new", "003-hello-there"]);
    assert!(t.path("work/001-hello-there").is_dir());
    t.flow(&["abandon"]);
    assert_in("no letters or digits", &t.flow_any(&["new", "!!!"]).stderr);
}

#[test]
fn commit_of_staged_docs_is_allowed_mid_feature() {
    let t = Flow::new();
    t.to_build();
    t.write("lib/app.ex", "wip\n");
    t.write("README.md", "more docs\n");
    t.git(&["add", "README.md"]);
    assert_eq!(t.bash("git commit -m 'docs: more'"), None);
    assert_eq!(t.bash("git commit -qm docs --author='a <a@b>'"), None);
    for c in [
        "git commit -am docs",
        "git commit -m docs lib/app.ex",
        "git commit -m docs -- README.md",
        "git add -A && git commit -m docs",
        "git commit -p -m docs",
        "git commit --all -m x",
        "git commit --pathspec-from=/tmp/list -m docs",
        "git -P commit -am docs",
        "git commit -m \"$(git add -A)\"",
        "git commit -m `git add -A`",
        "git commit -F <(git add -A)",
    ] {
        assert_in("committed only in ship", &t.bash(c).unwrap_or_default());
    }
    t.git(&["add", "lib/app.ex"]);
    assert_in("committed only in ship", &denied(t.bash("git commit -m docs")));
}

#[test]
fn paths_name_code_only_inside_the_repo() {
    let mut t = Flow::new();
    t.flow(&["new", "hello"]);
    let root = t.root.display().to_string();
    // The root through a symlink (macOS's /var is one), and under ~.
    let links = TempDir::new();
    symlink(&t.root, links.path().join("repo")).unwrap();
    let linked = links.path().join("repo").display().to_string();
    t.set_env("HOME", links.path());
    let code = "phase explore";
    for c in [
        format!("rm {root}/lib/app.ex"),
        format!("rm '{linked}/lib/app.ex'"),
        "rm ~/repo/lib/app.ex".into(),
        "rm -rf ./lib".into(),
        "rm \"$(pwd)/lib/x\"".into(),
    ] {
        assert_in(code, &denied(t.bash(&c)));
    }
    assert_in("state files", &denied(t.bash(&format!("rm {linked}/work/flow.json"))));
    assert_in("state files", &denied(t.bash(&format!("rm {root}/work/flow.json"))));
    for c in ["cp README.md /tmp/lib/README.md", "rm /x/mix.exs", "rm mix.exs.bak"] {
        assert_eq!(t.bash(c), None, "{c}");
    }
}

#[test]
fn each_repo_judges_what_is_done_in_it() {
    // The session is in this repo (phase explore); `other` is a second flow repo, with no
    // feature active.
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    let other = Flow::new();
    let o = other.root.display().to_string();
    let none = "there is no active feature";
    let edit_other = t.hook("Edit", json!({"file_path": other.path("lib/app.ex")}));
    assert_in("no active feature", &denied(edit_other));
    assert_in("phase explore", &denied(t.edit("lib/app.ex")));
    assert_in("no active feature", &denied(t.bash(&format!("cd {o} && echo x > lib/app.ex"))));
    assert_in("state files", &denied(t.bash(&format!("rm {o}/work/flow.json"))));
    // Code changed here doesn't stop a docs commit there, and code changed there is
    // judged there.
    t.write("lib/x.ex", "x\n");
    other.write("README.md", "more\n");
    assert_eq!(t.bash(&format!("cd {o} && git commit -m docs README.md")), None);
    other.git(&["add", "README.md"]);
    assert_eq!(t.bash(&format!("cd {o}; git commit -m docs")), None);
    other.write("lib/y.ex", "y\n");
    for c in [format!("cd {o} && git commit -am x"), format!("git -C {o} commit -am x")] {
        assert_in(none, &denied(t.bash(&c)));
    }
    // A `cd` back, or to a directory that isn't there, leaves the commands after it here.
    let back = format!("cd {o} && cd {} && echo x > lib/app.ex", t.root.display());
    assert_in("phase explore", &denied(t.bash(&back)));
    assert_in("phase explore", &denied(t.bash("cd /no/such/dir; echo x > lib/app.ex")));

    // A session outside any flow repo is still judged by the repo it acts in.
    let mut t = t;
    let outside = TempDir::new();
    t.set_env("GIT_CEILING_DIRECTORIES", outside.path().parent().unwrap());
    let edit = json!({"cwd": outside.path(), "tool_name": "Edit",
                      "tool_input": {"file_path": t.path("lib/app.ex")}});
    assert_in("phase explore", &denied(pre(&t, outside.path(), &edit.to_string())));
    let bash = json!({"cwd": outside.path(), "tool_name": "Bash",
                      "tool_input": {"command": format!("cd {} && mix format", t.root.display())}});
    assert_in("phase explore", &denied(pre(&t, outside.path(), &bash.to_string())));

    // A repo nested in the tree without a .flow.json (a dependency) is the outer repo's.
    let dep = t.path("lib/dep");
    fs::create_dir_all(&dep).unwrap();
    run(t.command("git").args(["init", "-q"]).current_dir(&dep), None);
    assert_in("phase explore", &denied(t.edit("lib/dep/x.ex")));
}

#[test]
fn pause_lifts_code_gates_but_not_state_protection() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    t.flow(&["pause"]);
    assert_eq!(t.edit("lib/app.ex"), None);
    assert_in("state files", &denied(t.edit("work/flow.json")));
    t.flow(&["resume"]);
    assert_in("phase explore", &denied(t.edit("lib/app.ex")));
}

#[test]
fn repos_without_flow_json_are_ignored() {
    let t = Flow::new();
    fs::remove_file(t.path(".flow.json")).unwrap();
    assert_eq!(t.edit("lib/app.ex"), None);
    assert_eq!(t.bash("git commit -am x"), None);
}

#[test]
fn session_start_reports_phase() {
    let t = Flow::new();
    t.flow(&["new", "hello", "Hello"]);
    let r = t.hook_raw("session-start", json!({"cwd": t.root}).to_string());
    let out = parse(&r.stdout);
    let ctx = out["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert_in("001-hello", ctx);
    assert_in("explore", ctx);
}

/// The context the session-start hook gives the agent.
fn session_context(t: &Flow) -> String {
    let r = t.hook_raw("session-start", json!({"cwd": t.root}).to_string());
    let ctx = &parse(&r.stdout)["hookSpecificOutput"]["additionalContext"];
    ctx.as_str().unwrap_or_else(|| panic!("no context: {}", r.stdout)).to_string()
}

#[test]
fn session_start_primes_the_agent() {
    let t = Flow::new();
    let cmd = flow_bin().display().to_string();
    let ctx = session_context(&t);
    for line in [
        &format!(
            "flow: no active feature. Code changes start with `{cmd} new <slug> \"<title>\"`.\n\n"
        ),
        "\nThis repo uses flow, a phase-gated feature workflow (explore -> spec -> tickets -> \
         build -> review -> ship -> done)",
        "Load the flow:flow skill before starting or continuing any change here.\n",
        "\nWhat flow keeps aligned:\n- Intent: ",
        "\n- Rules: while a feature is active, .flow.json, .claude/settings.json, \
         .claude/settings.local.json, work/STANDING.md, work/templates/ and work/specs/",
        "never edit work/flow.json or work/*/state.json",
        "\nProtected code here: lib, mix.exs. It is edited only in build and committed only \
         in ship.",
        "\nNow (no active feature): ",
        &format!("run `{cmd} new <slug> \"<title>\"`: that starts a feature in explore"),
        "\nKeep the user in the loop on flow: ",
        "every flow hook denial with its reason",
    ] {
        assert_in(line, &ctx);
    }

    // What to do now follows the phase, and the gates the user delegated.
    t.flow(&["new", "hello", "Hello"]);
    let ctx = session_context(&t);
    assert_in("\nNow (explore): fill work/001-hello/research.md. ", &ctx);
    assert!(!ctx.contains("Delegated to the judge"), "{ctx}");
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    assert_in(
        &format!("then ask the user for `! {cmd} approve spec`. No code edits yet."),
        &session_context(&t),
    );
    t.flow(&["delegate", "spec", "review"]);
    let ctx = session_context(&t);
    assert_in(&format!("Present it, then run `{cmd} judge`. No code edits yet."), &ctx);
    assert_in(
        &format!("\nDelegated to the judge: spec, review. For those, run `{cmd} judge`"),
        &ctx,
    );

    let t = Flow::new();
    t.to_build();
    assert_in("\nNow (build): per ticket, write the failing test first", &session_context(&t));
    let t = Flow::new();
    t.to_ship();
    let ctx = session_context(&t);
    assert_in(
        &format!(
            "\nNow (ship): commit the code together with work/001-hello/, then `{cmd} advance`"
        ),
        &ctx,
    );
    t.configure(json!({"protected_paths": []}));
    assert_in("\nProtected code: none is configured in .flow.json.\n", &session_context(&t));
}

// --- messages, exit codes, state files and hashes ----------------------------------

/// The strings of a JSON array.
fn strings(v: &Value) -> Vec<&str> {
    let items = v.as_array().unwrap_or_else(|| panic!("not an array: {v}"));
    items.iter().map(|s| s.as_str().unwrap()).collect()
}

/// The `at` of a state.json's history entry `i`.
fn history_at(state: &str, i: usize) -> String {
    let at = parse(state)["history"][i]["at"].as_str().unwrap().to_string();
    assert!(is_timestamp(&at), "{at}");
    at
}

/// The top-level `keys` appear in this order in a pretty-printed JSON object.
#[track_caller]
fn assert_key_order(text: &str, keys: &[&str]) {
    let at: Vec<usize> = keys
        .iter()
        .map(|k| {
            let key = format!("\n  \"{k}\": ");
            text.find(&key).unwrap_or_else(|| panic!("no {k:?} in {text}"))
        })
        .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{keys:?} out of order in {text}");
}

const DOC: &str = "flow — a phase-gated feature workflow for Claude Code.\n\n\
commands: init, new, status, check, advance, back, approve, delegate, judge, gates, deploy, pause, resume, abandon\n";

#[test]
fn help_and_usage_errors() {
    let mut t = Flow::new();
    let r = t.flow_any(&[]);
    assert_eq!((r.code, r.stdout.as_str(), r.stderr.as_str()), (0, DOC, ""));
    for args in [&["bogus"][..], &["hook"], &["--help"]] {
        let r = t.flow_any(args);
        assert_eq!((r.code, r.stdout.as_str()), (2, DOC), "{args:?}");
    }
    let r = t.hook_raw("nope", "{}");
    assert_eq!((r.code, r.stdout.as_str()), (0, ""));

    let err = |t: &Flow, args: &[&str]| {
        let r = t.flow_any(args);
        assert_eq!(r.stdout, "", "{args:?}");
        (r.code, r.stderr)
    };
    let usage = |s: &str| (1, format!("flow: {s}\n"));
    let new_usage = usage(r#"usage: flow new <slug> ["<title>"]"#);
    assert_eq!(err(&t, &["new"]), new_usage);
    assert_eq!(err(&t, &["deploy"]), usage("no deploy config in .flow.json"));
    t.flow(&["new", "hello"]);
    let approve_usage = usage("usage: flow approve spec|tickets|review");
    assert_eq!(err(&t, &["approve"]), approve_usage);
    assert_eq!(err(&t, &["approve", "spec", "tickets"]), approve_usage);
    assert_eq!(
        err(&t, &["approve", "tickets"]),
        usage("tickets can only be approved in phase tickets (now explore)")
    );
    assert_eq!(err(&t, &["back"]), usage("cannot go back from explore"));
    assert_eq!(err(&t, &["gates"]), usage("gates run in build (now explore)"));
    let todo = "- research.md: still contains TODO(flow) placeholders\n\
                - research.md: 1 unanswered item(s) in '## open questions'";
    assert_eq!(err(&t, &["advance"]), usage(&format!("cannot advance from explore:\n{todo}")));
    let r = t.flow_any(&["check"]);
    assert_eq!((r.code, r.stdout), (1, format!("{todo}\n")));
    assert_eq!(
        t.flow(&["status"]).stdout,
        "001-hello — Hello\n\
         phase: [explore] > spec > tickets > build > review > ship > done\n\
         to advance:\n  - research.md: still contains TODO(flow) placeholders\n  \
         - research.md: 1 unanswered item(s) in '## open questions'\n"
    );

    fs::remove_file(t.path(".flow.json")).unwrap();
    let not_flow = "flow: no .flow.json at the git root; this repo does not use flow \
                    (`flow init` opts it in)\n";
    assert_eq!(err(&t, &["status"]), (2, not_flow.to_string()));
    let outside = TempDir::new();
    t.set_env("GIT_CEILING_DIRECTORIES", outside.path().parent().unwrap());
    let r = run(t.command(flow_bin()).arg("init").current_dir(outside.path()), None);
    assert_eq!((r.code, r.stderr.as_str()), (1, "flow: not inside a git repository\n"));
}

#[test]
fn gates_output_and_results() {
    let t = Flow::new();
    t.configure(json!({"standing_gates": ["echo hi", "exit 3"]}));
    t.to_build();
    let r = t.flow_any(&["gates"]);
    assert_eq!(r.code, 1);
    // Each "== [...]" line is flushed before its command runs, so they interleave.
    assert_eq!(
        r.stdout,
        "\n== [standing] echo hi\nhi\n\n== [standing] exit 3\n== FAILED (exit 3)\n\n\
         == [T01] test -f lib/app.ex\n== FAILED (exit 1)\n\ngates FAILED (3 command(s))\n"
    );
    let gates = &t.state()["gates"];
    assert_eq!(gates["passed"], false);
    assert_eq!(
        gates["results"],
        json!([
            {"source": "standing", "command": "echo hi", "exit": 0},
            {"source": "standing", "command": "exit 3", "exit": 3},
            {"source": "T01", "command": "test -f lib/app.ex", "exit": 1},
        ])
    );
    assert!(is_timestamp(gates["at"].as_str().unwrap()));
    assert_in("gates: FAILED\n", &t.flow(&["status"]).stdout);
}

#[test]
fn state_files_are_pretty_printed_in_a_fixed_key_order() {
    // Indented by 2 with a trailing newline, the known keys in flow's order.
    let t = Flow::new();
    t.flow(&["new", "hello", "Hello"]);
    assert_eq!(
        t.read("work/flow.json"),
        "{\n  \"active\": \"001-hello\",\n  \"paused\": false\n}\n"
    );
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    t.fwrite("spec.md", SPEC);
    t.flow(&["approve", "spec"]);
    let st = t.fread("state.json");
    let (at0, at1) = (history_at(&st, 0), history_at(&st, 1));
    let approved = t.state()["approvals"]["spec"]["at"].as_str().unwrap().to_string();
    assert_eq!(
        st,
        format!(
            r#"{{
  "id": "001-hello",
  "title": "Hello",
  "phase": "spec",
  "approvals": {{
    "spec": {{
      "hash": "93e3dea9a6bf3616",
      "at": "{approved}"
    }}
  }},
  "gates": null,
  "history": [
    {{
      "phase": "explore",
      "at": "{at0}"
    }},
    {{
      "phase": "spec",
      "at": "{at1}"
    }}
  ]
}}
"#
        )
    );
    t.flow(&["back"]);
    let back = &t.state()["history"][2];
    assert_eq!((&back["phase"], &back["back"]), (&json!("explore"), &json!(true)));
}

#[test]
fn non_ascii_titles_are_written_as_utf8() {
    let t = Flow::new();
    let title = "Olá ☃ 𝄞";
    let r = t.flow(&["new", "hello", title]);
    assert_eq!(
        r.stdout,
        "started 001-hello in phase explore; fill in work/001-hello/research.md\n"
    );
    assert_eq!(t.state()["title"], title);
    assert!(t.fread("research.md").starts_with("# Research: Olá ☃ 𝄞\n"));
    assert_in("001-hello — Olá ☃ 𝄞\n", &t.flow(&["status"]).stdout);

    // Each hook prints one JSON line.
    let r = t.hook_raw("session-start", json!({"cwd": t.root}).to_string());
    assert_eq!(r.stdout.lines().count(), 1, "{}", r.stdout);
    assert_eq!(
        parse(&r.stdout),
        json!({"hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": session_context(&t),
        }})
    );
    assert!(session_context(&t).starts_with(
        "flow: active feature 001-hello (Olá ☃ 𝄞) is in phase explore. To advance: \
         research.md: still contains TODO(flow) placeholders; research.md: 1 unanswered \
         item(s) in '## open questions'\n\n"
    ));
    let payload = json!({
        "cwd": t.root, "tool_name": "Write",
        "tool_input": {"file_path": t.path("work/flow.json")},
    });
    let r = t.hook_raw("pre-tool-use", payload.to_string());
    assert_eq!(r.stdout.lines().count(), 1, "{}", r.stdout);
    assert_eq!(
        parse(&r.stdout),
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "flow: state files are written only by the flow CLI",
        }})
    );

    // Slugs keep only [a-z0-9-]; the default title comes from the slug.
    t.flow(&["abandon"]);
    let r = t.flow(&["new", "Olá Wörld"]);
    assert_eq!(
        r.stdout,
        "started 002-ol-w-rld in phase explore; fill in work/002-ol-w-rld/research.md\n"
    );
    assert_eq!(parse(&t.read("work/002-ol-w-rld/state.json"))["title"], "Ol w rld");
    assert_eq!(t.phase(), "abandoned");
}

#[test]
fn unknown_keys_survive_a_cli_write() {
    let t = Flow::new();
    t.flow(&["new", "hello", "Hello"]);
    // Known keys in the order flow writes them, unknown ones in between.
    let state = r#"{"id": "001-hello", "custom": {"z": 1, "a": [true, null, "é"]},
        "title": "Hello", "phase": "explore", "approvals": {}, "gates": null,
        "history": [{"phase": "explore", "at": "2026-01-01T00:00:00Z", "who": "me"}],
        "zzz": 0.5}"#;
    t.fwrite("state.json", state);
    let global = r#"{"note": "képt", "active": "001-hello", "paused": false,
                     "extra": [1, {"b": []}]}"#;
    t.write("work/flow.json", global);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    t.flow(&["pause"]);

    let st = t.fread("state.json");
    let mut want = parse(state);
    want["phase"] = json!("spec");
    let at = history_at(&st, 1);
    want["history"].as_array_mut().unwrap().push(json!({"phase": "spec", "at": at}));
    assert_eq!(parse(&st), want);
    assert_key_order(&st, &["id", "title", "phase", "approvals", "gates", "history"]);
    assert!(st.ends_with("}\n"), "{st}");

    let g = t.read("work/flow.json");
    let mut want = parse(global);
    want["paused"] = json!(true);
    assert_eq!(parse(&g), want);
    assert_key_order(&g, &["active", "paused"]);
}

#[test]
fn missing_and_null_keys_stay_as_they_were() {
    let t = Flow::new();
    t.flow(&["new", "hello", "Hello"]);
    let mut st = t.state();
    let fields = st.as_object_mut().unwrap();
    fields.remove("approvals");
    fields.remove("gates");
    fields.insert("reviewed_fingerprint".into(), Value::Null);
    t.fwrite("state.json", &st.to_string());
    t.write("work/flow.json", r#"{"active": "001-hello"}"#);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    let after = t.state();
    let keys: Vec<&str> = after.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["id", "title", "phase", "history", "reviewed_fingerprint"]);
    assert_eq!(after["reviewed_fingerprint"], Value::Null);
    t.flow(&["back"]);
    t.flow(&["abandon"]);
    assert_eq!(parse(&t.read("work/flow.json")), json!({"active": null}));
}

#[test]
fn documents_split_lines_at_unicode_line_breaks() {
    // A line also ends at \v, \f, \x1c-\x1e, NEL and U+2028/2029, and \x1c-\x1f count
    // as whitespace.
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    for sep in ["\u{b}", "\u{c}", "\u{1c}", "\u{85}", "\u{2028}", "\u{2029}"] {
        t.fwrite("research.md", &RESEARCH.replace("\n", sep));
        assert_eq!(t.flow(&["check"]).stdout, "explore: complete\n", "{sep:?}");
    }
    t.fwrite("research.md", &RESEARCH.replace("## Options\n", "##\u{1f}Options\n"));
    assert_eq!(t.flow(&["check"]).stdout, "explore: complete\n");
}

#[test]
fn init_writes_settings_and_config_json() {
    let t = Flow::new();
    fs::remove_file(t.path(".flow.json")).unwrap();
    t.write("mix.exs", "defmodule X.MixProject do end\n");
    let settings = "{\"env\": {\"GREETING\": \"olá 𝄞\", \"olá\": \"\\u0000\\u0007\\b\\f\\n\\r\\t\\u001f\\u007f/\\\\\\\"~ \"},\n \
         \"permissions\": {\"allow\": [\"Bash(ls:*)\"], \"deny\": [\"Bash(flow approve:*)\"], \"ask\": []},\n \
         \"n\": 1, \"f\": 1.5, \"t\": true, \"z\": null, \"o\": {}}";
    t.write(".claude/settings.json", settings);
    let r = t.flow(&["init"]);
    assert_eq!(
        r.stdout,
        "wrote .flow.json (defaults for mix.exs), flow/STANDING.md and deny rules in .claude/settings.json\n\
         next: check protected_paths, standing_gates and codegen in .flow.json, fill in flow/STANDING.md, \
         and add a deploy section if ship should wait for CI\n"
    );
    // Every key kept; the deny rules added after the existing ones.
    let mut want = parse(settings);
    want["permissions"]["deny"] = json!([
        "Bash(flow approve:*)",
        "Bash(flow delegate:*)",
        "Bash(flow pause:*)",
        "Bash(flow resume:*)",
        "Bash(flow abandon:*)",
        "Edit(flow/flow.json)",
        "Edit(flow/*/state.json)",
    ]);
    let written = t.read(".claude/settings.json");
    assert_eq!(parse(&written), want);
    assert_key_order(&written, &["env", "permissions", "n", "f", "t", "z", "o"]);
    assert_eq!(
        t.read(".flow.json"),
        r#"{
  "work_dir": "flow",
  "protected_paths": [
    "lib/",
    "test/",
    "config/",
    "priv/",
    "assets/",
    "mix.exs",
    "mix.lock"
  ],
  "standing_gates": [
    "mix compile --warnings-as-errors",
    "mix format --check-formatted",
    "mix test"
  ],
  "codegen": [
    "\\bmix\\s+(phx|ecto)\\.gen\\.\\S",
    "\\bmix\\s+igniter\\.\\S",
    "\\bmix\\s+format\\b(?!.*--check-formatted)"
  ]
}
"#
    );
    let template = read(&checkout("templates/STANDING.md"));
    let name = t.root.file_name().unwrap().to_str().unwrap();
    assert_eq!(
        t.read("flow/STANDING.md"),
        template.replace("{project}", name).replace("{work_dir}", "flow")
    );

    // No known stack, no settings yet; a settings file that is not JSON stops init.
    let u = Flow::new();
    fs::remove_file(u.path(".flow.json")).unwrap();
    u.write(".claude/settings.json", "{\"permissions\": // no\n}");
    let r = u.flow_any(&["init"]);
    let path = fs::canonicalize(&u.root).unwrap().join(".claude/settings.json");
    let msg = format!("flow: {} is not plain JSON; nothing written\n", path.display());
    assert_eq!((r.code, r.stderr), (1, msg));
    assert!(!u.path(".flow.json").exists());
    fs::remove_file(u.path(".claude/settings.json")).unwrap();
    assert_in("(no known stack: guessed)", &u.flow(&["init"]).stdout);
    assert_eq!(
        u.read(".flow.json"),
        "{\n  \"work_dir\": \"flow\",\n  \"protected_paths\": [\n    \"src/\"\n  ],\n  \
         \"standing_gates\": [],\n  \"codegen\": []\n}\n"
    );
    assert_eq!(
        u.read(".claude/settings.json"),
        "{\n  \"permissions\": {\n    \"deny\": [\n      \"Bash(flow approve:*)\",\n      \
         \"Bash(flow delegate:*)\",\n      \"Bash(flow pause:*)\",\n      \
         \"Bash(flow resume:*)\",\n      \"Bash(flow abandon:*)\",\n      \
         \"Edit(flow/flow.json)\",\n      \"Edit(flow/*/state.json)\"\n    ]\n  }\n}\n"
    );
}

#[test]
fn codegen_defaults_and_the_real_repos_patterns_work() {
    // Each stack's defaults from `flow init`, then the patterns the repos using flow add
    // (~/Work/atomize's .flow.json), judged in phase explore.
    let stacks: [(&str, &[&str], &[&str]); 4] = [
        (
            "mix.exs",
            &["mix phx.gen.live A b c", "mix igniter.install x", "mix format"],
            &["mix format --check-formatted", "mix phx.server"],
        ),
        ("Cargo.toml", &["cargo fmt"], &["cargo fmt --check", "cargo build"]),
        (
            "pyproject.toml",
            &["ruff format .", "black src"],
            &["ruff format --check .", "black --check src"],
        ),
        (
            "package.json",
            &["npx prettier --write .", "eslint --fix src"],
            &["prettier --check .", "eslint src"],
        ),
    ];
    for (marker, codegen, other) in stacks {
        let t = Flow::new();
        fs::remove_file(t.path(".flow.json")).unwrap();
        t.write(marker, "\n");
        t.flow(&["init"]);
        t.flow(&["new", "hello"]);
        for c in codegen {
            assert_in("phase explore", &denied(t.bash(c)));
        }
        for c in other {
            assert_eq!(t.bash(c), None, "{marker}: {c}");
        }
    }
    let t = Flow::new();
    t.configure(json!({"codegen": [
        r"\bmix\s+(phx|ecto)\.gen\.\S",
        r"\bmix\s+igniter\.\S",
        r"\bmix\s+format\b(?!.*--check-formatted)",
        r"\bmix\s+precommit\b",
        r"\bmix\s+deps\.unlock\b(?!.*--check-unused)",
    ]}));
    t.flow(&["new", "hello"]);
    for c in ["mix precommit", "mix deps.unlock --all", "mix ecto.gen.migration x"] {
        assert_in("phase explore", &denied(t.bash(c)));
    }
    assert_eq!(t.bash("mix deps.unlock --check-unused && mix test"), None);
}

#[test]
fn fingerprint_and_artifact_hashes_are_stable() {
    // Features in progress depend on these exact values: state.json records the spec and
    // tickets hashes and the gates' and review's fingerprints, and a different value voids
    // the approvals or blocks the commit. They must never change.
    // Git runs without the user's global config, whose excludes could change the tree.
    const FINGERPRINT: &str = "8ad0d0bd4df44ad6";
    const SPEC_HASH: &str = "958b176291b35f8a";
    const TICKETS_HASH: &str = "8b31f0bfd9350ec9";

    let t = Flow::with_env(&[("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")]);
    t.write(".gitignore", "*.log\n");
    t.write("docs/gone.md", "tracked, then deleted\n");
    t.git(&["add", "-A"]);
    t.git(&["commit", "-qm", "tree"]);
    fs::remove_file(t.path("docs/gone.md")).unwrap(); // <deleted>
    t.write("debug.log", "ignored\n");
    t.write("workshop.txt", "not the work dir\n");
    t.write("work/notes.md", "inside the work dir\n");
    t.write("lib/olá.ex", "olá\n");
    t.write("lib/a b.ex", "spaced\n");
    t.write("lib/bin.dat", [0xff, 0x00, 0x80, b'\n']);
    t.write("lib/app.ex", "hello\n");
    symlink("app.ex", t.path("lib/link.ex")).unwrap();
    symlink("../nowhere", t.path("lib/dangling.ex")).unwrap();
    symlink("../work", t.path("lib/dirlink")).unwrap();

    // The spec as features approved it before `## Spec changes`, under the template of then.
    let template = include_str!("../templates/spec.md");
    let (before, after) = template.split_once("## Spec changes").unwrap();
    t.write(
        "work/templates/spec.md",
        format!("{before}{}", &after[after.find("## Out").unwrap()..]),
    );
    t.flow(&["new", "hello", "Hello"]);
    t.fwrite("research.md", RESEARCH);
    t.flow(&["advance"]);
    t.fwrite("spec.md", SPEC_V1);
    t.flow(&["approve", "spec"]);
    t.flow(&["advance"]);
    t.fwrite("tickets/T01.md", TICKET);
    let t02 = TICKET.replace("# T01: Show hello", "# T02: Show it again");
    t.fwrite("tickets/T02.md", &t02);
    t.flow(&["approve", "tickets"]);
    t.flow(&["advance"]);
    t.flow(&["gates"]);
    t.flow(&["advance"]);
    t.fwrite("review.md", REVIEW);
    t.flow(&["advance"]);

    let st = t.state();
    assert_eq!(st["gates"]["fingerprint"], FINGERPRINT);
    assert_eq!(st["reviewed_fingerprint"], FINGERPRINT);
    assert_eq!(st["approvals"]["spec"]["hash"], SPEC_HASH);
    assert_eq!(st["approvals"]["tickets"]["hash"], TICKETS_HASH);
    assert_eq!(st["phase"], "ship");
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Verdict {
    Allowed,
    Code,
    State,
    User,
    Commit,
}

#[test]
fn bash_edge_cases() {
    // flow's verdicts on commands at the edges of the shell splitting, heredoc, redirect,
    // write-hint, codegen and git rules, in phase explore with an uncommitted lib/x.ex.
    // Some allow what they arguably should not (README, "Known limitations").
    use Verdict::*;
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    t.write("lib/x.ex", "x\n");
    let cases = [
        ("echo x &>lib/app.ex", Allowed),
        ("echo x &> lib/app.ex", Allowed),
        ("exec 3<>lib/app.ex", Allowed),
        ("echo x >>lib/app.ex", Code),
        ("echo x 1>lib/app.ex", Code),
        ("echo x >'lib/app.ex'", Code),
        ("echo x > \"lib/a b.ex\"", Code),
        ("echo x>lib/app.ex", Code),
        ("echo x >| lib/app.ex", Allowed),
        ("echo x > lib", Code),
        ("echo x > ../outside/lib/app.ex", Allowed),
        ("echo x > ./lib/app.ex", Code),
        ("echo x > /dev/null 2>&1 && ls lib", Allowed),
        ("cd lib && echo x > app.ex", Allowed),
        ("echo é > lib/é.ex", Code),
        ("echo 'a;b' > /tmp/x; cat lib/app.ex", Allowed),
        ("echo \"a\\\"b > lib/app.ex\"", Code),
        ("echo \"a\\\"b\" > lib/app.ex", Code),
        ("echo a\\;b > /tmp/x", Allowed),
        ("echo 'unterminated > lib/app.ex", Code),
        ("echo \\\n> lib/app.ex", Code),
        ("cat <<-EOF > lib/app.ex\n\tx\n\tEOF", Code),
        ("cat <<-EOF > /tmp/x\n\trm -rf lib\n\tEOF\nls", Allowed),
        ("python3 - <<'PY'\nopen('lib/app.ex','w').write('x')\nPY", Allowed),
        ("sh <<EOF\nrm -rf lib\nEOF", Code),
        ("cat > /tmp/x <<\"EOF\"\nrm -rf lib\nEOF", Allowed),
        ("sh <<EOF\nrm lib/app.ex", Code),
        ("cat <<A > /tmp/a; sh <<B\nx\nA\nrm lib/app.ex\nB", Code),
        ("cat <<EOF | sh\nrm lib/app.ex\nEOF", Code),
        ("cat <<'EOF' |& bash\nrm -rf lib\nEOF", Code),
        ("cat <<EOF || true\nrm lib/app.ex\nEOF", Allowed),
        ("cat <<< foo\necho x > lib/app.ex", Code),
        ("echo $((1<<2))\necho x > lib/app.ex", Code),
        ("echo $(( 1 << 2 ))\nrm lib/app.ex", Code),
        ("X=1 cat <<EOF > /tmp/x\nrm -rf lib\nEOF", Allowed),
        ("tee lib/app.ex < /dev/null", Code),
        ("cat f | tee -a lib/app.ex", Code),
        ("perl -pi -e s/a/b/ lib/app.ex", Allowed),
        ("perl -i -pe s/a/b/ lib/app.ex", Code),
        ("sed -n -i s/a/b/ mix.exs", Code),
        ("sed -e s/a/b/ mix.exs", Allowed),
        ("git -C lib checkout .", Allowed),
        ("git checkout -- lib/app.ex", Code),
        ("git stash", Allowed),
        ("git restore --staged lib", Code),
        ("ln -s x lib/y", Code),
        ("touch libfoo", Allowed),
        ("touch ./lib", Code),
        ("touch src/lib", Allowed),
        ("rm -rf lib.bak", Allowed),
        ("rm -rf /tmp/lib/cache", Allowed),
        ("rm $PWD/lib/app.ex", Code),
        ("rm ${PWD}/lib/app.ex", Code),
        ("rm lib*", Code),
        ("mv a 'lib/b'", Code),
        ("cp x=lib/y z", Code),
        ("cp --target-directory=lib a", Code),
        ("cp --target-directory=./lib a", Code),
        ("/bin/rm lib/app.ex", Code),
        ("docker run --rm -e DIR=lib img", Allowed),
        ("docker run --rm img sh -c 'make lib'", Allowed),
        ("docker run -e DIR=lib img", Allowed),
        ("x --cp lib", Allowed),
        ("rm-cache lib", Allowed),
        ("truncate -s0 mix.exs", Code),
        ("rm mix.exs.orig", Allowed),
        ("rm mix_exs", Allowed),
        ("mix format --check-formatted lib/x.ex; mix format", Code),
        ("mix ecto.gen.migration add", Code),
        ("MIX_ENV=test mix format", Code),
        ("mix igniter.install foo", Code),
        ("mix phx.gen", Allowed),
        ("mix formatter", Allowed),
        ("mix format --check-formatted", Allowed),
        ("flow  approve spec", User),
        ("./flow approve", User),
        ("myflow approve", Allowed),
        ("flow approved", Allowed),
        ("flow approve\u{301} spec", User),
        ("flow approve_x", Allowed),
        ("(flow pause)", User),
        ("x=$(flow resume)", User),
        ("echo 'flow abandon'", User),
        ("flow\tabandon", User),
        ("FLOW=1 flow pause", User),
        ("/usr/bin/flow approve tickets", User),
        ("flowapprove", Allowed),
        ("rm work/001-hello/state.json.bak", Allowed),
        ("rm /tmp/work/flow.json", Allowed),
        ("echo > work/001-hello/state.json.bak", Allowed),
        ("echo x > work/flow.json.tmp", Allowed),
        ("rm work/x/y/state.json", Allowed),
        ("cp a work/002-x/state.json", State),
        ("echo x > work//flow.json", State),
        ("echo x > ./work/flow.json", State),
        ("echo x > work/001-hello/../flow.json", State),
        ("echo x >> work/flow.json", State),
        ("ls work/flow.json && rm -f work/flow.json", State),
        ("git push", Allowed),
        // A lone commit takes only what is staged: lib/x.ex is not.
        ("git commit -m x", Allowed),
        ("git commit -am x", Commit),
        ("git commit -m \"$(git add -A)\"", Commit),
        ("git -c a=b commit -m x", Commit),
        ("git --no-pager commit", Commit),
        ("git -P commit -am x", Commit),
        ("git --git-dir .git commit -am x", Commit),
        ("git commit --pathspec-from=f -m x", Commit),
        ("git commit-tree x", Commit),
        ("git log --grep commit", Allowed),
        ("echo git commit", Commit),
        ("gitcommit", Allowed),
    ];
    let code = "flow: 001-hello is in phase explore; code is edited only in build";
    let state = "flow: state files are written only by the flow CLI";
    let commit = "flow: commit touches code (lib/x.ex) but 001-hello is in phase explore; \
                  code is committed only in ship";
    let user = "flow: approve/delegate/pause/resume/abandon are the user's: they run them as `! ";
    let wrong: Vec<String> = cases
        .into_iter()
        .filter_map(|(c, want)| {
            let got = match t.bash(c) {
                None => Allowed,
                Some(r) if r == code => Code,
                Some(r) if r == state => State,
                Some(r) if r == commit => Commit,
                Some(r) if r.starts_with(user) && r.ends_with(" ...`") => User,
                Some(r) => panic!("{c:?}: unexpected reason {r:?}"),
            };
            (got != want).then(|| format!("{c:?}: {got:?}, expected {want:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The pre-tool-use verdict on a raw payload, the hook running in `cwd`.
#[track_caller]
fn pre(t: &Flow, cwd: &Path, stdin: &str) -> Option<String> {
    verdict(&t.hook_in(cwd, "pre-tool-use", stdin))
}

#[track_caller]
fn assert_allowed(t: &Flow, cwd: &Path, stdin: &str) {
    assert_eq!(pre(t, cwd, stdin), None, "{stdin}");
}

#[track_caller]
fn assert_hook_error(t: &Flow, cwd: &Path, stdin: &str) {
    let reason = denied(pre(t, cwd, stdin));
    assert!(reason.starts_with("flow: hook error: "), "{stdin}: {reason}");
}

#[test]
fn a_trailing_slash_names_the_file_a_link_points_at() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    t.write("lib/x.ex", "x\n");
    // A link in lib to a file outside it.
    symlink("../README.md", t.path("lib/alias.ex")).expect("symlink");
    assert_eq!(t.bash("echo x > lib/alias.ex/"), None);
    assert_in("phase explore", &denied(t.bash("echo x > lib/x.ex/")));
}

#[test]
fn malformed_payloads_fail_closed() {
    let mut t = Flow::new();
    t.flow(&["new", "hello"]);
    let root = t.root.display().to_string();
    let here = t.root.clone();
    // Stdin that is not JSON fails closed when the hook runs in a flow repo.
    let reason = denied(pre(&t, &here, "not json"));
    assert_eq!(reason, "flow: hook error: expected ident at line 1 column 2");
    assert_hook_error(&t, &here, "");
    assert_hook_error(&t, &here, "{\"cwd\": ");
    // Payloads of the wrong shape fail closed.
    assert_hook_error(&t, &here, "[1, 2]");
    assert_hook_error(&t, &here, "null");
    let tool = |name: &str, input: &str| {
        format!(r#"{{"cwd": "{root}", "tool_name": {name}, "tool_input": {input}}}"#)
    };
    assert_hook_error(&t, &here, &tool(r#""Edit""#, r#""lib/app.ex""#));
    assert_hook_error(&t, &here, &tool(r#""Edit""#, r#"{"file_path": 5}"#));
    assert_hook_error(&t, &here, &tool(r#""Bash""#, r#"{"command": 5}"#));
    assert_hook_error(&t, &here, &tool(r#""Bash""#, r#"{"command": null}"#));
    assert_hook_error(&t, &here, r#"{"cwd": 5}"#);
    let gone = t.path("no/such/dir").display().to_string();
    assert_hook_error(&t, &here, &format!(r#"{{"cwd": "{gone}"}}"#));
    // Empty values fall back to their defaults.
    assert_allowed(&t, &here, &tool(r#""Edit""#, "[]"));
    assert_allowed(&t, &here, &tool(r#""Edit""#, "null"));
    assert_allowed(&t, &here, &tool(r#""Bash""#, "{}"));
    assert_allowed(&t, &here, &tool("5", "{}"));
    let notebook = r#"{"cwd": null, "tool_name": "Edit",
                       "tool_input": {"file_path": null, "notebook_path": "lib/a.ex"}}"#;
    assert_eq!(
        pre(&t, &here, notebook).as_deref(),
        Some("flow: 001-hello is in phase explore; code is edited only in build")
    );

    // session-start says nothing on errors.
    for stdin in ["x", "", "[1]"] {
        let ss = t.hook_raw("session-start", stdin);
        assert_eq!((ss.code, ss.stdout.as_str(), ss.stderr.as_str()), (0, "", ""), "{stdin}");
    }

    // A state.json that is not JSON denies a call that reads it.
    let state = t.fread("state.json");
    t.fwrite("state.json", "{");
    assert_hook_error(&t, &here, &tool(r#""Edit""#, r#"{"file_path": "lib/app.ex"}"#));
    t.fwrite("state.json", &state);

    // So does a .flow.json that is not JSON, whatever the hook's own cwd.
    t.write(".flow.json", "{");
    let elsewhere = TempDir::new();
    let readme = format!(r#"{{"file_path": "{root}/README.md"}}"#);
    assert_hook_error(&t, elsewhere.path(), &tool(r#""Edit""#, &readme));
    let ss = t.hook_in(elsewhere.path(), "session-start", format!(r#"{{"cwd": "{root}"}}"#));
    assert_eq!((ss.code, ss.stdout.as_str(), ss.stderr.as_str()), (0, "", ""));

    // Outside any flow repo, stdin that is not JSON is allowed; a payload of the wrong
    // shape still denies.
    let outside = TempDir::new();
    t.set_env("GIT_CEILING_DIRECTORIES", outside.path().parent().unwrap());
    assert_allowed(&t, outside.path(), "not json");
    assert_allowed(&t, outside.path(), "");
    assert_hook_error(&t, outside.path(), "[1]");
}

#[test]
fn a_cwd_the_hook_cannot_enter_fails_closed() {
    let t = Flow::new();
    let locked = t.path("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let payload = json!({"cwd": locked, "tool_name": "Edit", "tool_input": {"file_path": "x"}});
    let r = t.hook_raw("pre-tool-use", payload.to_string());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    if fs::read_dir("/root").is_ok() {
        return; // root can enter it anyway
    }
    assert_in("flow: hook error: ", &denied(verdict(&r)));
}

#[test]
fn a_long_command_is_judged_not_failed() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    let long = format!("echo {}", "a".repeat(100_000));
    assert_eq!(t.bash(&long), None);
    let lines: String =
        (0..2000).map(|n| format!("print({n})  # a generated line, long enough\n")).collect();
    assert_eq!(t.bash(&format!("python3 - <<'PY'\n{lines}PY")), None);
    let write = t.bash(&format!("{long} > lib/app.ex"));
    assert_in("code is edited only in build", &denied(write));
}

#[test]
fn flow_self_names_the_cli_when_flow_is_not_on_path() {
    if testing_launcher() {
        eprintln!("skipped: bin/flow sets FLOW_SELF to its own path");
        return;
    }
    let mut t = Flow::new();
    t.set_env("PATH", path_without_flow());
    let no_feature = |t: &Flow| {
        let r = t.flow_any(&["check"]);
        assert_eq!(r.code, 1);
        r.stderr
    };

    // Inside the repo: its path relative to the root.
    t.write("tools/flow", "#!/bin/sh\n");
    t.set_env("FLOW_SELF", t.path("tools/flow"));
    assert_eq!(
        no_feature(&t),
        "flow: no active feature; start one with `tools/flow new <slug> \"<title>\"`\n"
    );
    assert_eq!(
        denied(t.bash("flow approve spec")),
        "flow: approve/delegate/pause/resume/abandon are the user's: they run them as `! tools/flow ...`"
    );

    // Outside it: the absolute path, symlinks resolved.
    let other = TempDir::new();
    let outside = other.path().join("bin/flow");
    write(&outside, "#!/bin/sh\n");
    let want = format!(
        "flow: no active feature; start one with `{} new <slug> \"<title>\"`\n",
        fs::canonicalize(&outside).unwrap().display()
    );
    t.set_env("FLOW_SELF", &outside);
    assert_eq!(no_feature(&t), want);
    symlink(&outside, t.path("tools/link")).unwrap();
    t.set_env("FLOW_SELF", t.path("tools/link"));
    assert_eq!(no_feature(&t), want);

    // A `flow` on PATH wins when it runs this one: as a link, or a script naming it,
    // expanded from the environment. Another flow on PATH does not.
    let stubs = TempDir::new();
    let stub = stubs.path().join("flow");
    let mut path = stubs.path().as_os_str().to_owned();
    path.push(":");
    path.push(path_without_flow());
    t.set_env("PATH", path);
    let bare = "flow: no active feature; start one with `flow new <slug> \"<title>\"`\n";
    write(&stub, "#!/bin/sh\necho another flow\n");
    chmod_x(&stub);
    assert_eq!(no_feature(&t), want);
    write(&stub, format!("#!/bin/sh\nexec \"{}\" \"$@\"\n", outside.display()));
    assert_eq!(no_feature(&t), bare);
    t.set_env("FLOW_TEST_DIR", other.path());
    write(&stub, "#!/bin/sh\nexec \"${FLOW_TEST_DIR}/bin/flow\" \"$@\"\n");
    assert_eq!(no_feature(&t), bare);
    fs::remove_file(&stub).unwrap();
    symlink(&outside, &stub).unwrap();
    chmod_x(&outside);
    assert_eq!(no_feature(&t), bare);
}

// --- text that is not UTF-8: an error, never a pass ----------------------------------

#[test]
fn stdin_that_is_not_utf8_is_denied_in_a_flow_repo() {
    let mut t = Flow::new();
    t.set_env("LC_ALL", "C"); // the locale changes nothing: flow reads UTF-8
    t.flow(&["new", "hello"]);
    let payload = format!(
        r#"{{"cwd": "{}", "tool_name": "Bash", "tool_input": {{"command": "rm lib/app.ex "#,
        t.root.display()
    );
    let mut payload = payload.into_bytes();
    payload.extend_from_slice(b"\xff\"}}");
    denied(verdict(&t.hook_raw("pre-tool-use", payload)));
}

#[test]
fn arguments_that_are_not_utf8_are_an_error() {
    let t = Flow::new();
    let mut c = t.command(flow_bin());
    c.arg("new").arg("x").arg(std::ffi::OsStr::from_bytes(b"T\xff"));
    assert_eq!(run(&mut c, None).code, 1);
    assert!(!t.path("work/flow.json").exists()); // no feature started
}

#[test]
fn a_symlink_target_that_is_not_utf8_fails_the_gates() {
    let t = Flow::new();
    t.to_build();
    symlink(std::ffi::OsStr::from_bytes(b"tgt\xff"), t.path("lnk")).unwrap();
    assert_eq!(t.flow_any(&["gates"]).code, 1);
    assert_eq!(t.state()["gates"], Value::Null);
}

// --- deploy over HTTP ----------------------------------------------------------------

/// One request a stub server received.
#[derive(Debug)]
struct Request {
    line: String,
    authorization: Option<String>,
    user_agent: Option<String>,
}

/// A stub HTTP server on 127.0.0.1: one request per connection, answered from its path.
/// /old/<rest> redirects to /<rest>; /api/ needs `Authorization: token sekret` and gives
/// one running Forgejo task; /status/<code> answers <code>; anything else is a 404.
fn serve() -> (u16, mpsc::Receiver<Request>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local addr").port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let Ok(reader) = conn.try_clone() else {
                continue;
            };
            let mut lines = BufReader::new(reader).lines().map_while(Result::ok);
            let line = lines.next().unwrap_or_default();
            let mut req = Request { line, authorization: None, user_agent: None };
            for header in lines.take_while(|h| !h.is_empty()) {
                let Some((name, value)) = header.split_once(':') else {
                    continue;
                };
                let value = Some(value.trim().to_string());
                match name.to_ascii_lowercase().as_str() {
                    "authorization" => req.authorization = value,
                    "user-agent" => req.user_agent = value,
                    _ => {}
                }
            }
            // Logged before the reply, so the test sees it once the client has its answer.
            let reply = respond(&req);
            let _ = tx.send(req);
            let _ = conn.write_all(reply.as_bytes());
        }
    });
    (port, rx)
}

fn respond(req: &Request) -> String {
    let path = req.line.split(' ').nth(1).unwrap_or("/");
    let reply = |status: &str, extra: &str, body: &str| {
        format!(
            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    if let Some(rest) = path.strip_prefix("/old") {
        return reply("301 Moved Permanently", &format!("Location: {rest}\r\n"), "");
    }
    if let Some(rest) = path.strip_prefix("/status/") {
        let code = rest.split('/').next().unwrap_or_default();
        return reply(&format!("{code} Whatever"), "", "");
    }
    if path.starts_with("/list/") {
        let runs = json!([{"id": 1, "name": "deploy", "status": "running", "head_sha": "abc"}]);
        return reply("200 OK", "", &runs.to_string());
    }
    if !path.starts_with("/api/") {
        return reply("404 Not Found", "", "");
    }
    if req.authorization.as_deref() != Some("token sekret") {
        return reply("401 Unauthorized", "", "");
    }
    let runs = json!({"workflow_runs": [
        {"id": 1, "name": "deploy", "status": "running", "head_sha": "abc"},
    ]});
    reply("200 OK", "", &runs.to_string())
}

/// A repo whose deploy reads Forgejo at `url`, with no proxy from the test's environment.
fn forgejo_repo(url: &str, health_url: Option<&str>) -> Flow {
    let mut t = Flow::new();
    for k in [
        "http_proxy",
        "HTTP_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
        "no_proxy",
        "NO_PROXY",
    ] {
        t.unset_env(k);
    }
    t.set_env("FORGEJO_TOKEN", "sekret");
    let mut deploy = json!({"forgejo_url": url, "repo": "edo/x"});
    if let Some(h) = health_url {
        deploy["health_url"] = json!(h);
    }
    t.configure(json!({ "deploy": deploy }));
    t
}

#[test]
fn forgejo_keeps_the_authorization_header_across_a_same_host_redirect() {
    let (port, requests) = serve();
    let t = forgejo_repo(&format!("http://127.0.0.1:{port}/old"), None);
    let r = t.flow(&["deploy"]);
    assert_in(" 1 deploy         running   abc\n", &r.stdout);
    let seen: Vec<Request> = requests.try_iter().collect();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen[0].line.starts_with("GET /old/api/v1/repos/edo/x/actions/tasks?limit=15 "));
    assert!(seen[1].line.starts_with("GET /api/v1/repos/edo/x/actions/tasks?limit=15 "));
    for req in &seen {
        assert_eq!(req.authorization.as_deref(), Some("token sekret"), "{seen:?}");
    }
}

#[test]
fn health_is_the_http_status_code() {
    let (port, requests) = serve();
    let base = format!("http://127.0.0.1:{port}");
    for (path, want) in [("/status/200", "200"), ("/status/503", "503"), ("/nope", "404")] {
        let t = forgejo_repo(&base, Some(&format!("{base}{path}")));
        assert_in(&format!("\nhealth: {want}\n"), &t.flow(&["deploy"]).stdout);
    }
    let health: Vec<Request> = requests.try_iter().filter(|r| r.authorization.is_none()).collect();
    assert_eq!(health.len(), 3, "{health:?}");
    for req in &health {
        assert_eq!(req.user_agent.as_deref(), Some("flow-health/1"), "{health:?}");
    }
    // No answer at all is 0.
    let t = forgejo_repo(&base, Some("http://127.0.0.1:1/"));
    assert_in("\nhealth: 0\n", &t.flow(&["deploy"]).stdout);
}

#[test]
fn forgejo_http_errors_fail_closed() {
    let (port, _requests) = serve();
    let t = forgejo_repo(&format!("http://127.0.0.1:{port}/status/503"), None);
    let r = t.flow_any(&["deploy"]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.starts_with("flow: cannot read Forgejo: "), "{}", r.stderr);
    let push = denied(t.bash("git push"));
    assert_in("cannot check whether a deploy is running (cannot read Forgejo: ", &push);
}

/// Reads one request's head from `conn`; false when the client closed it first.
fn read_head(conn: &mut BufReader<std::net::TcpStream>) -> bool {
    let mut line = String::new();
    let mut got = false;
    while conn.read_line(&mut line).is_ok_and(|n| n > 0) {
        got = true;
        if line == "\r\n" {
            return true;
        }
        line.clear();
    }
    got
}

#[test]
fn health_follows_a_redirect_on_a_new_connection() {
    // An HTTP/1.0 reply with no Connection header ends the connection, but this server
    // holds it open and drops it when a second request arrives on it, which always fails
    // a client that reuses the connection.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        let mut first = true;
        for conn in listener.incoming().flatten() {
            let redirect = std::mem::replace(&mut first, false);
            std::thread::spawn(move || {
                let mut conn = BufReader::new(conn);
                if !read_head(&mut conn) {
                    return;
                }
                let reply = if redirect {
                    "HTTP/1.0 302 Found\r\nLocation: /ok\r\nContent-Length: 0\r\n\r\n"
                } else {
                    "HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok"
                };
                let _ = conn.get_mut().write_all(reply.as_bytes());
                // A second request on this connection gets no answer.
                read_head(&mut conn);
            });
        }
    });
    let base = format!("http://127.0.0.1:{port}");
    let t = forgejo_repo(&base, None);
    let health_url = format!("{base}/redirect");
    let deploy =
        json!({"provider": "command", "status_command": "echo []", "health_url": health_url});
    t.configure(json!({ "deploy": deploy }));
    assert_in("health: 200\n", &t.flow(&["deploy"]).stdout);
}

// --- symlinks, heredoc words, payloads and very long input --------------------------

#[test]
fn symlinks_resolve_as_realpath_does() {
    let t = Flow::new();
    t.write("lib/a.ex", "x\n");
    // A link to a file not there yet, one to a missing directory, one reached after
    // `..` steps through a missing directory or a file, and a loop.
    symlink("lib/new.ex", t.path("alias.ex")).unwrap();
    symlink("lib/sub", t.path("aliasdir")).unwrap();
    symlink("lib", t.path("link")).unwrap();
    symlink("loop2", t.path("loop1")).unwrap();
    symlink("loop1", t.path("loop2")).unwrap();
    let none = "flow: no active feature; code changes go through `";
    for rel in ["alias.ex", "aliasdir/x.ex", "nonexist/../link/x.ex", "README.md/../link/x.ex"] {
        assert_in(none, &denied(t.edit(rel)));
    }
    assert_in(none, &denied(t.bash("echo hi > alias.ex")));
    assert_eq!(t.edit("loop1/x.ex"), None);
    assert_eq!(t.edit("nonexist/../README.md"), None);
}

#[test]
fn a_heredoc_word_is_letters_numbers_and_underscores() {
    let t = Flow::new();
    let none = "flow: no active feature; code changes go through `";
    // Bash ends these heredocs at `EOF²`, `E²` and `EOF\u{301}`, and runs what follows.
    for cmd in [
        "cat <<EOF²\nEOF²\nrm -rf lib\n",
        "cat > /tmp/x <<E²\nE²\necho hi > lib/a.ex\n",
        "cat <<EOF\u{301}\nEOF\nrm -rf lib\n",
    ] {
        assert_in(none, &denied(t.bash(cmd)));
    }
    assert_eq!(t.bash("cat <<'EOF'\nrm -rf lib\nEOF\nls"), None);
}

#[test]
fn a_payload_that_is_not_strict_json_fails_closed() {
    // Claude Code sends JSON. What serde_json rejects (a lone surrogate escape, which
    // JavaScript's JSON.stringify can emit; NaN, Infinity, numbers beyond f64) is denied
    // in a flow repo rather than judged.
    let t = Flow::new();
    let root = t.root.display().to_string();
    let none = "flow: no active feature; code changes go through `";
    let payload = |command: &str, extra: &str| {
        format!(
            r#"{{"cwd": "{root}", "tool_name": "Bash", "tool_input": {{"command": "{command}"{extra}}}}}"#
        )
    };
    // A surrogate pair is one character, judged as any other.
    assert_in(none, &denied(pre(&t, &t.root, &payload(r"rm -rf lib # 😀", ""))));
    let reason = denied(pre(&t, &t.root, &payload(r"rm -rf lib # \udc00\ud800x", "")));
    assert_in("flow: hook error: lone leading surrogate in hex escape", &reason);
    let big = format!(r#", "timeout": {}"#, "9".repeat(400));
    for stdin in [
        payload(r"rm -rf lib # \ud800", ""),
        payload(r"ls # \udc00", ""),
        payload("ls", r#", "timeout": NaN"#),
        payload("ls", r#", "timeout": [-Infinity, Infinity]"#),
        payload("ls", r#", "timeout": 1e400"#),
        payload("ls", &big),
        payload("ls", r#", "timeout": nan"#),
    ] {
        assert_hook_error(&t, &t.root, &stdin);
    }
}

#[test]
fn a_flow_json_that_is_not_an_object_denies() {
    let t = Flow::new();
    for config in ["[]", r#"["work", ["lib/"]]"#, "null"] {
        t.write(".flow.json", config);
        let reason = denied(t.edit("README.md"));
        assert!(reason.starts_with("flow: hook error: "), "{config}: {reason}");
    }
    t.write(".flow.json", r#"{"deploy": ["forgejo"]}"#);
    assert!(denied(t.edit("README.md")).starts_with("flow: hook error: "));
}

#[test]
fn a_very_long_command_is_judged() {
    let t = Flow::new();
    t.flow(&["new", "hello"]);
    assert_eq!(t.bash(&format!("echo {}", "a b ".repeat(125_000))), None);
    let body = "print(1)\n".repeat(150_000);
    assert_eq!(t.bash(&format!("python3 - <<EOF\n{body}EOF")), None);
    let format = format!("echo {} ; mix format", "a b ".repeat(125_000));
    assert_in("phase explore", &denied(t.bash(&format)));
}

#[test]
fn a_forgejo_reply_that_is_not_an_object_fails_closed() {
    let (port, _requests) = serve();
    let t = forgejo_repo(&format!("http://127.0.0.1:{port}/list"), None);
    let push = denied(t.bash("git push"));
    assert_in("cannot check whether a deploy is running (forgejo: expected a list", &push);
    assert_eq!(t.flow_any(&["deploy"]).code, 1);
}
