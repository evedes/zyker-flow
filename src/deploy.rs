//! CI runs from the configured provider, and the health check.

use crate::config::{Deploy, StatusCommand};
use crate::error::{bail, Error, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fmt::Display;
use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PROVIDERS: &str = "forgejo, github, command";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// A CI run, whatever the provider.
#[derive(Deserialize)]
pub struct Run {
    #[serde(default = "no_id")]
    pub id: Value,
    pub name: String,
    pub status: String,
    pub head_sha: String,
}

/// CI run statuses across providers: anything not in flight or passed is a failure.
const IN_FLIGHT: [&str; 7] =
    ["running", "waiting", "blocked", "queued", "pending", "requested", "in_progress"];

impl Run {
    pub fn in_flight(&self) -> bool {
        IN_FLIGHT.contains(&self.status.as_str())
    }

    pub fn passed(&self) -> bool {
        ["success", "skipped"].contains(&self.status.as_str())
    }

    /// The id as `flow deploy` prints it: a string as is, null as `None`, booleans as
    /// `True`/`False`, anything else as JSON.
    pub fn id(&self) -> String {
        match &self.id {
            Value::String(s) => s.clone(),
            Value::Null => "None".into(),
            Value::Bool(b) => if *b { "True" } else { "False" }.into(),
            v => v.to_string(),
        }
    }
}

fn no_id() -> Value {
    Value::String(String::new())
}

/// `name=status, ...`
pub fn run_list<'a>(runs: impl IntoIterator<Item = &'a Run>) -> String {
    let parts: Vec<String> = runs.into_iter().map(|r| format!("{}={}", r.name, r.status)).collect();
    parts.join(", ")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRun {
    database_id: Value,
    name: String,
    status: String,
    conclusion: Option<String>,
    head_sha: String,
}

fn set(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

/// The latest `limit` CI runs.
pub fn runs(deploy: &Deploy, root: &Path, limit: u32) -> Result<Vec<Run>> {
    let provider = match set(&deploy.provider) {
        Some(p) => p,
        None if deploy.forgejo_url.is_some() => "forgejo",
        None => bail!("deploy.provider must be one of {PROVIDERS}"),
    };
    let shape = || Error(format!("{provider}: expected a list of {{name, status, head_sha}}"));
    let runs = match provider {
        "forgejo" => match forgejo(deploy, &format!("/actions/tasks?limit={limit}"))? {
            Value::Object(mut tasks) => {
                tasks.remove("workflow_runs").unwrap_or(Value::Array(vec![]))
            }
            _ => return Err(shape()),
        },
        "github" => {
            let mut cmd = Command::new("gh");
            cmd.args(["run", "list", "--limit", &limit.to_string()]);
            cmd.args(["--json", "databaseId,name,status,conclusion,headSha"]);
            if let Some(repo) = set(&deploy.repo) {
                cmd.args(["--repo", repo]);
            }
            let gh: Vec<GhRun> =
                serde_json::from_value(run_json(cmd, root, "gh run list")?).map_err(|_| shape())?;
            let run = |r: GhRun| Run {
                id: r.database_id,
                name: r.name,
                // A completed run with no conclusion shows as None.
                status: if r.status == "completed" {
                    r.conclusion.unwrap_or_else(|| "None".into())
                } else {
                    r.status
                },
                head_sha: r.head_sha,
            };
            return Ok(gh.into_iter().map(run).collect());
        }
        "command" => {
            let cmd = match &deploy.status_command {
                Some(StatusCommand::Shell(s)) if !s.is_empty() => {
                    let mut c = Command::new("/bin/sh");
                    c.arg("-c").arg(s);
                    c
                }
                Some(StatusCommand::Argv(argv)) if !argv.is_empty() => {
                    let mut c = Command::new(&argv[0]);
                    c.args(&argv[1..]);
                    c
                }
                _ => bail!("deploy.provider command needs a deploy.status_command"),
            };
            run_json(cmd, root, "deploy.status_command")?
        }
        _ => bail!("deploy.provider must be one of {PROVIDERS}"),
    };
    serde_json::from_value(runs).map_err(|_| shape())
}

/// An HTTP client: system roots, the environment's proxy, a 15 s timeout. No idle
/// connections are kept: a redirect would otherwise be sent on the connection of the
/// response, which an HTTP/1.0 server has closed.
fn agent(any_status: bool) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier);
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .tls_config(tls.build())
        .max_idle_connections(0)
        .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
        .http_status_as_error(!any_status)
        .max_redirects_will_error(!any_status)
        .build()
        .into()
}

fn forgejo(deploy: &Deploy, path: &str) -> Result<Value> {
    let token_env = deploy.token_env.as_deref().unwrap_or("FORGEJO_TOKEN");
    let token = std::env::var(token_env).unwrap_or_default();
    if token.is_empty() {
        bail!("${token_env} is not set; cannot read Forgejo");
    }
    let (Some(base), Some(repo)) = (&deploy.forgejo_url, &deploy.repo) else {
        bail!("deploy needs a forgejo_url and a repo to read Forgejo");
    };
    let url = format!("{}/api/v1/repos/{repo}{path}", base.trim_end_matches('/'));
    let fail = |e: &dyn Display| Error(format!("cannot read Forgejo: {e}"));
    let call = agent(false).get(&url).header("Authorization", format!("token {token}")).call();
    let body = call.and_then(|mut r| r.body_mut().read_to_vec()).map_err(|e| fail(&e))?;
    serde_json::from_slice(&body).map_err(|e| fail(&e))
}

/// The health URL's HTTP status after redirects (0 when it cannot be reached); None
/// without a health_url.
pub fn health(deploy: &Deploy) -> Option<u16> {
    let url = set(&deploy.health_url)?;
    // Cloudflare answers 403 to some default User-Agents.
    let call = agent(true).get(url).header("User-Agent", "flow-health/1").call();
    Some(call.map_or(0, |r| r.status().as_u16()))
}

/// A child's exit code; minus the signal that killed it.
pub fn exit_code(status: ExitStatus) -> i32 {
    status.code().or_else(|| status.signal().map(|s| -s)).unwrap_or(-1)
}

/// Reads `pipe` to the end on a thread.
fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// Waits for `child` for at most `timeout` (until it exits and closes its output): its
/// status, stdout and stderr (empty when not piped), or None once it was killed for
/// taking longer.
pub fn finish(mut child: Child, timeout: Duration) -> Result<Option<(ExitStatus, [Vec<u8>; 2])>> {
    let deadline = Instant::now() + timeout;
    let pipes = [drain(child.stdout.take()), drain(child.stderr.take())];
    let status = loop {
        if pipes.iter().all(JoinHandle::is_finished) {
            if let Some(status) = child.try_wait()? {
                break status;
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(5));
    };
    Ok(Some((status, pipes.map(|h| h.join().unwrap_or_default()))))
}

/// Runs `cmd` in `root` for at most 30 seconds (until it exits and closes its output)
/// and parses its stdout as JSON.
fn run_json(mut cmd: Command, root: &Path, what: &str) -> Result<Value> {
    let fail = |e: &dyn Display| Error(format!("{what} failed: {e}"));
    cmd.current_dir(root).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = cmd.spawn().map_err(|e| fail(&e))?;
    let Some((status, [stdout, stderr])) = finish(child, COMMAND_TIMEOUT).map_err(|e| fail(&e))?
    else {
        return Err(fail(&format!("timed out after {} seconds", COMMAND_TIMEOUT.as_secs())));
    };
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let stderr: String = stderr.trim().chars().take(200).collect();
        bail!("{what} failed (exit {}): {stderr}", exit_code(status));
    }
    serde_json::from_slice(&stdout).map_err(|_| Error(format!("{what} did not print JSON")))
}
