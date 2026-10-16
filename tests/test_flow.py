"""End-to-end tests for the flow CLI and its hooks, against a throwaway git repo.

Run: python3 -m unittest discover -s plugins/flow/tests
"""

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

FLOW = Path(__file__).resolve().parent.parent / "bin" / "flow"

RESEARCH = """# Research: x
## Question
What to build.
## Findings
Nothing yet.
## Options
Do it.
## Open questions
- [x] Colour? Blue.
"""
SPEC = """# Spec: x
## Goal
A greeting.
## Behaviour
Shows hello.
## Data model
None.
## UI
None.
## Acceptance criteria
Hello is shown.
## Out of scope
Goodbyes.
## Open questions
None.
"""
TICKET = """# T01: Show hello
## Description / desired end state
Hello shown.
## Context / why
Spec goal.
## Entry points
lib/app.ex
## In scope
lib/
## Out of scope
config/
## Gate
lib/app.ex exists.
## Verification steps
```sh
test -f lib/app.ex
```
"""
REVIEW = """# Review: x
## Verdict
pass — matches the spec.
## Findings
None.
"""


class FlowTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "t@example.com")
        self.git("config", "user.name", "t")
        (self.root / ".flow.json").write_text(json.dumps({
            "work_dir": "work",
            "protected_paths": ["lib/", "mix.exs"],
            "standing_gates": ["true"],
        }))
        (self.root / "README.md").write_text("readme\n")
        self.git("add", "-A")
        self.git("commit", "-qm", "init")

    def tearDown(self):
        self.tmp.cleanup()

    # helpers
    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, capture_output=True, text=True, check=True).stdout

    def flow(self, *args, ok=True):
        r = subprocess.run([str(FLOW), *args], cwd=self.root, capture_output=True, text=True)
        if ok:
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        return r

    def hook(self, tool, **tool_input):
        payload = {"cwd": str(self.root), "tool_name": tool, "tool_input": tool_input}
        r = subprocess.run([str(FLOW), "hook", "pre-tool-use"], input=json.dumps(payload),
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        if not r.stdout.strip():
            return None
        out = json.loads(r.stdout)["hookSpecificOutput"]
        self.assertEqual(out["permissionDecision"], "deny")
        return out["permissionDecisionReason"]

    def edit(self, rel):
        return self.hook("Edit", file_path=str(self.root / rel))

    def fdir(self):
        return self.root / "work" / "001-hello"

    def to_build(self):
        self.flow("new", "hello", "Hello")
        (self.fdir() / "research.md").write_text(RESEARCH)
        self.flow("advance")
        (self.fdir() / "spec.md").write_text(SPEC)
        self.flow("approve", "spec")
        self.flow("advance")
        (self.fdir() / "tickets" / "T01.md").write_text(TICKET)
        self.flow("approve", "tickets")
        self.flow("advance")

    def to_ship(self):
        self.to_build()
        (self.root / "lib").mkdir(exist_ok=True)
        (self.root / "lib" / "app.ex").write_text("hello\n")
        self.flow("gates")
        self.flow("advance")
        (self.fdir() / "review.md").write_text(REVIEW)
        self.flow("advance")

    def phase(self):
        return json.loads((self.fdir() / "state.json").read_text())["phase"]

    # tests
    def test_new_scaffolds_research_and_blocks_code(self):
        self.flow("new", "hello", "Hello")
        self.assertTrue((self.fdir() / "research.md").is_file())
        self.assertIn("phase explore", self.edit("lib/app.ex"))
        self.assertIsNone(self.edit("README.md"))
        self.assertIn("still active", self.flow("new", "other", ok=False).stderr)

    def test_placeholders_and_open_questions_block_advance(self):
        self.flow("new", "hello")
        r = self.flow("advance", ok=False)
        self.assertIn("TODO(flow)", r.stderr)
        (self.fdir() / "research.md").write_text(RESEARCH.replace("[x]", "[ ]"))
        self.assertIn("unanswered", self.flow("advance", ok=False).stderr)

    def test_spec_needs_valid_approval(self):
        self.flow("new", "hello")
        (self.fdir() / "research.md").write_text(RESEARCH)
        self.flow("advance")
        self.assertTrue((self.fdir() / "spec.md").is_file())
        self.assertIn("not complete", self.flow("approve", "spec", ok=False).stderr)
        (self.fdir() / "spec.md").write_text(SPEC)
        self.assertIn("approve spec", self.flow("advance", ok=False).stderr)
        self.flow("approve", "spec")
        (self.fdir() / "spec.md").write_text(SPEC + "\nchanged\n")
        self.assertIn("approve spec", self.flow("advance", ok=False).stderr)
        self.flow("approve", "spec")
        self.flow("advance")
        self.assertEqual(self.phase(), "tickets")

    def test_tickets_need_verification_commands(self):
        self.flow("new", "hello")
        (self.fdir() / "research.md").write_text(RESEARCH)
        self.flow("advance")
        (self.fdir() / "spec.md").write_text(SPEC)
        self.flow("approve", "spec")
        self.flow("advance")
        no_cmds = TICKET.replace("test -f lib/app.ex", "")
        (self.fdir() / "tickets" / "T01.md").write_text(no_cmds)
        self.assertIn("no commands", self.flow("check", ok=False).stdout)

    def test_build_allows_code_and_gates_go_stale(self):
        self.to_build()
        self.assertIsNone(self.edit("lib/app.ex"))
        self.assertIn("gates", self.flow("advance", ok=False).stderr)
        r = self.flow("gates", ok=False)  # lib/app.ex does not exist yet
        self.assertNotEqual(r.returncode, 0)
        (self.root / "lib").mkdir(exist_ok=True)
        (self.root / "lib" / "app.ex").write_text("hello\n")
        self.flow("gates")
        (self.root / "lib" / "app.ex").write_text("changed\n")
        self.assertIn("gates", self.flow("advance", ok=False).stderr)

    def test_state_files_and_user_commands_are_protected(self):
        self.to_build()
        self.assertIn("state files", self.edit("work/001-hello/state.json"))
        self.assertIn("state files", self.edit("work/flow.json"))
        self.assertIn("state files", self.hook("Bash", command="echo {} > work/001-hello/state.json"))
        self.assertIsNone(self.hook("Bash", command="cat work/001-hello/state.json"))
        for c in ("flow approve spec", "plugins/flow/bin/flow pause", "flow abandon"):
            self.assertIn("user's", self.hook("Bash", command=c))
        self.assertIsNone(self.hook("Bash", command="flow status"))

    def test_bash_writes_to_code_follow_the_phase(self):
        self.flow("new", "hello")
        self.assertIn("phase explore", self.hook("Bash", command="echo x > lib/app.ex"))
        self.assertIn("phase explore", self.hook("Bash", command="sed -i s/a/b/ mix.exs"))
        self.assertIn("phase explore", self.hook("Bash", command="mix phx.gen.live Blog Post posts"))
        self.assertIsNone(self.hook("Bash", command="grep -r hello lib/ 2>/dev/null"))
        self.assertIsNone(self.hook("Bash", command="ls lib/ >/dev/null 2>&1"))

    def test_commit_gate(self):
        (self.root / "docs.md").write_text("docs\n")
        self.assertIsNone(self.hook("Bash", command="git commit -am docs"))
        (self.root / "lib").mkdir(exist_ok=True)
        (self.root / "lib" / "x.ex").write_text("x\n")
        self.assertIn("no active feature", self.hook("Bash", command="git commit -am x"))
        (self.root / "lib" / "x.ex").unlink()

        self.to_ship()
        self.assertIsNone(self.hook("Bash", command="git add -A && git commit -m hello"))
        (self.root / "lib" / "app.ex").write_text("sneaky\n")
        self.assertIn("gates", self.hook("Bash", command="git commit -am hello"))

    def test_review_fail_blocks_and_back_returns_to_build(self):
        self.to_build()
        (self.root / "lib").mkdir(exist_ok=True)
        (self.root / "lib" / "app.ex").write_text("hello\n")
        self.flow("gates")
        self.flow("advance")
        self.assertTrue((self.fdir() / "review.md").is_file())
        (self.fdir() / "review.md").write_text(REVIEW.replace("pass —", "fail —"))
        self.assertIn("Verdict", self.flow("advance", ok=False).stderr)
        self.flow("back")
        self.assertEqual(self.phase(), "build")

    def test_ship_to_done_after_commit(self):
        self.to_ship()
        self.assertIn("uncommitted", self.flow("advance", ok=False).stderr)
        self.git("add", "-A")
        self.git("commit", "-qm", "hello")
        self.flow("advance")
        self.assertEqual(self.phase(), "done")
        self.flow("new", "next")  # active slot is free again

    def test_pause_lifts_code_gates_but_not_state_protection(self):
        self.flow("new", "hello")
        self.flow("pause")
        self.assertIsNone(self.edit("lib/app.ex"))
        self.assertIn("state files", self.edit("work/flow.json"))
        self.flow("resume")
        self.assertIn("phase explore", self.edit("lib/app.ex"))

    def test_repos_without_flow_json_are_ignored(self):
        (self.root / ".flow.json").unlink()
        self.assertIsNone(self.edit("lib/app.ex"))
        self.assertIsNone(self.hook("Bash", command="git commit -am x"))

    def test_session_start_reports_phase(self):
        self.flow("new", "hello", "Hello")
        r = subprocess.run([str(FLOW), "hook", "session-start"], input=json.dumps({"cwd": str(self.root)}),
                           capture_output=True, text=True)
        ctx = json.loads(r.stdout)["hookSpecificOutput"]["additionalContext"]
        self.assertIn("001-hello", ctx)
        self.assertIn("explore", ctx)


if __name__ == "__main__":
    unittest.main()
