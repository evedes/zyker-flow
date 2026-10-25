//! The primer the session-start hook gives the agent in a flow repo: what flow is, what
//! it holds the work to, where things stand and what to do now. The skill has the
//! detail; this is what every session gets whether or not the skill is loaded.

use crate::config::{Phase, State};
use crate::judge::GATES;
use crate::repo::Repo;

/// The primer after the status line; `active` is the active feature, when there is one
/// and its state could be read.
pub fn primer(repo: &Repo, active: Option<(&str, &State)>) -> String {
    let cmd = crate::paths::flow_cmd(&repo.root);
    let work = &repo.work_rel;
    let mut out = vec![
        "This repo uses flow, a phase-gated feature workflow (explore -> spec -> tickets -> \
         build -> review -> ship -> done) whose order the flow hooks enforce on every edit and \
         shell command. Load the flow:flow skill before starting or continuing any change here."
            .into(),
        "What flow keeps aligned:".into(),
        "- Intent: code follows a spec and tickets the user approved. Each approval is bound \
         to the artifact's content hash, so editing an approved spec.md or ticket voids it."
            .into(),
        "- Code: what is committed is exactly the code that passed the gates and review (a \
         fingerprint of every file git sees), so any change after them needs them again."
            .into(),
        format!(
            "- Rules: while a feature is active, .flow.json, .claude/settings.json, \
             .claude/settings.local.json, {work}/STANDING.md, {work}/templates/ and \
             {work}/specs/ change only by the user's hand."
        ),
        format!(
            "- Authority: approve, delegate, pause, resume and abandon are the user's (`! {cmd} \
             approve spec`). Never run them, and never edit {work}/flow.json or \
             {work}/*/state.json."
        ),
    ];
    if repo.protected.is_empty() {
        out.push("Protected code: none is configured in .flow.json.".into());
    } else {
        out.push(format!(
            "Protected code here: {}. It is edited only in build and committed only in ship. \
             Other files (docs, CI) can be changed any time, but count towards the gates' \
             fingerprint.",
            repo.protected.join(", ")
        ));
    }
    out.push(match active {
        None => format!(
            "Now (no active feature): reading, research and discussion are free, and so are \
             changes outside protected code. Before changing protected code, run `{cmd} new \
             <slug> \"<title>\"`: that starts a feature in explore. The rule files above can be \
             edited now, and the next feature runs under what they say then, so change them \
             only when the user asks."
        ),
        Some((fid, st)) => now(repo, &cmd, fid, st),
    });
    if let Some((_, st)) = active {
        let delegated: Vec<&str> =
            GATES.iter().filter(|g| st.delegated(**g).is_some()).map(|g| g.name()).collect();
        if !delegated.is_empty() {
            out.push(format!(
                "Delegated to the judge: {}. For those, run `{cmd} judge` (Bash timeout \
                 600000 ms) instead of asking the user for the approval.",
                delegated.join(", ")
            ));
        }
    }
    out.push(format!(
        "Keep the user in the loop on flow: in a line or two, say which phase the work is in \
         and what that allows, each `{cmd} advance` or `{cmd} back` and why, what blocks the \
         next phase, which approval you need from them and what it binds, and every flow hook \
         denial with its reason. A denial is flow holding the work to what was agreed: fix \
         its cause or ask the user, never route around it."
    ));
    out.join("\n")
}

/// What to do in the active feature's phase.
fn now(repo: &Repo, cmd: &str, fid: &str, st: &State) -> String {
    let dir = format!("{}/{fid}", repo.work_rel);
    let ask = |gate: Phase| match st.delegated(gate) {
        Some(_) => format!("run `{cmd} judge`"),
        None => format!("ask the user for `! {cmd} approve {gate}`"),
    };
    let phase = st.phase;
    let what = match phase {
        Phase::Explore => format!(
            "fill {dir}/research.md. Read the living spec ({}/specs/, if there is one) and the \
             code first; put decisions only the user can make under `## Open questions` as \
             `- [ ]` items, then stop and ask. No code edits yet",
            repo.work_rel
        ),
        Phase::Spec => format!(
            "fill {dir}/spec.md: the user-visible behaviour, testable acceptance criteria, what \
             is out of scope and `## Spec changes`. Present it, then {}. No code edits yet",
            ask(Phase::Spec)
        ),
        Phase::Tickets => format!(
            "split the approved spec into {dir}/tickets/T01-<slug>.md, T02-..., each with \
             runnable `## Verification steps`, then {}. Leave spec.md alone: editing it voids \
             its approval",
            ask(Phase::Tickets)
        ),
        Phase::Build => format!(
            "per ticket, write the failing test first, then the code, then check it in the \
             running app. When every ticket is done, run `{cmd} gates`; any file change after \
             that makes them stale"
        ),
        Phase::Review => match st.delegated(Phase::Review) {
            Some(_) => format!(
                "run `{cmd} judge`: it reviews the code and writes {dir}/review.md. Change \
                 nothing now; on a rejection, `{cmd} back` to build"
            ),
            None => format!(
                "have a fresh subagent that did not write the code review spec.md, the \
                 tickets and `git diff` into {dir}/review.md. Change nothing now; on fail, \
                 `{cmd} back` to build"
            ),
        },
        Phase::Ship => match repo.config.deploy {
            Some(_) => format!(
                "commit the code together with {dir}/, push, and watch `{cmd} deploy`; \
                 `{cmd} advance` to done once CI is green for HEAD and the health check \
                 passes, then commit {}/ once more",
                repo.work_rel
            ),
            None => format!(
                "commit the code together with {dir}/, then `{cmd} advance` to done and \
                 commit {}/ once more",
                repo.work_rel
            ),
        },
        Phase::Done | Phase::Abandoned => "nothing: the feature is closed".into(),
    };
    format!("Now ({phase}): {what}.")
}
