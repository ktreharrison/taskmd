use crate::model::TaskCard;
use crate::store::TaskStore;
use anyhow::{Context, Result};
use chrono::Local;
use std::path::Path;
use std::process::Command;

/// Most bytes kept from each of an eval's stdout and stderr; the rest is drained and dropped.
const EVAL_CAPTURE_LIMIT: usize = 1 << 20;
/// After the eval's shell exits, how long background children may keep its output open.
const EVAL_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// A task has an eval only if its command is non-blank (`eval: ""` is no eval at all).
pub(crate) fn has_eval(card: &TaskCard) -> bool {
    card.meta
        .eval
        .as_deref()
        .is_some_and(|e| !e.trim().is_empty())
}

/// Run a command, killing it and every process it started if it outlives `timeout`.
/// The deadline covers the whole process group and output collection, not just the shell:
/// a background child that keeps stdout open cannot stretch the eval past its timeout.
/// Returns `Ok(None)` on timeout. Output is drained on threads and capped per stream.
pub(crate) fn run_with_timeout(
    mut cmd: Command,
    timeout: Option<std::time::Duration>,
) -> std::io::Result<Option<std::process::Output>> {
    use std::io::Read;
    use std::process::Stdio;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    fn drain(pipe: Option<Box<dyn Read + Send>>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            if let Some(mut p) = pipe {
                let mut buf = [0u8; 8192];
                while let Ok(n) = p.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let room = EVAL_CAPTURE_LIMIT.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
            kept
        })
    }
    let out_t = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err_t = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );

    let start = std::time::Instant::now();
    let mut status = None;
    let mut exited_at: Option<std::time::Instant> = None;
    let timed_out = loop {
        if status.is_none()
            && let Some(st) = child.try_wait()?
        {
            status = Some(st);
            exited_at = Some(std::time::Instant::now());
        }
        if status.is_some() && out_t.is_finished() && err_t.is_finished() {
            break false;
        }
        if timeout.is_some_and(|t| start.elapsed() >= t) {
            break true;
        }
        // No timeout configured: still stop background children that outlive the shell.
        if timeout.is_none() && exited_at.is_some_and(|t| t.elapsed() >= EVAL_DRAIN_GRACE) {
            kill_process_tree(&mut child);
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    if timed_out {
        kill_process_tree(&mut child);
        let _ = child.wait();
    }
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    if timed_out {
        return Ok(None);
    }
    Ok(status.map(|status| std::process::Output {
        status,
        stdout,
        stderr,
    }))
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        const SIGKILL: i32 = 9;
        // The child leads its own process group, so -pid reaches everything it spawned.
        unsafe { kill(-(child.id() as i32), SIGKILL) };
    }
    let _ = child.kill();
}

impl TaskStore {
    /// Runs the task's `eval` shell command (if configured) and records pass/fail on the card.
    pub fn run_eval(&self, query: &str, actor: &str) -> Result<(TaskCard, bool, String)> {
        let found = self.find_task(query)?;
        let _lock = Self::lock_card(Path::new(&found.path))?;
        let mut card = Self::read_card(Path::new(&found.path))?;
        card.scope = found.scope;
        let (passed, output) = self.eval_card(&mut card, actor)?;
        self.write_card(&card)?;
        let cur_status = card.meta.status.as_str().to_string();
        self.log_event(
            &card.meta.id,
            "eval",
            actor,
            card.meta.session_id.as_deref(),
            Some(&cur_status),
            &cur_status,
            Some(passed),
            &card.meta.title,
        )?;
        Ok((card, passed, output))
    }

    /// Evaluate a card the caller has already locked and re-read. Mutates the card; does not write it.
    pub(crate) fn eval_card(&self, card: &mut TaskCard, actor: &str) -> Result<(bool, String)> {
        let now_ts = Local::now().format("%Y-%m-%d %H:%M").to_string();

        let Some(cmd_str) = card.meta.eval.clone().filter(|e| !e.trim().is_empty()) else {
            card.meta.eval_status = Some("passed (no-eval)".to_string());
            return Ok((true, "No eval command configured (auto-pass)".to_string()));
        };

        #[cfg(target_os = "windows")]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&cmd_str);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let mut cmd = {
            let shell = if let Ok(s) = std::env::var("SHELL")
                && !s.is_empty()
                && std::path::Path::new(&s).exists()
            {
                s
            } else if std::path::Path::new("/bin/bash").exists() {
                "/bin/bash".to_string()
            } else if std::path::Path::new("/bin/zsh").exists() {
                "/bin/zsh".to_string()
            } else {
                "sh".to_string()
            };
            let mut c = Command::new(shell);
            c.arg("-c").arg(&cmd_str);
            c
        };

        cmd.current_dir(&self.config.vault_path);
        let timeout = match self.config.policy.eval_timeout_secs {
            0 => None,
            secs => Some(std::time::Duration::from_secs(secs)),
        };
        let output = run_with_timeout(cmd, timeout)
            .with_context(|| format!("Failed to execute eval command: {}", cmd_str))?;
        let Some(output) = output else {
            let secs = self.config.policy.eval_timeout_secs;
            card.meta.eval_status = Some("failed".to_string());
            card.body = format!(
                "{}\n- `{}`: Eval `{}` run by `{}` -> **TIMED OUT ⏱** (after {}s).",
                card.body.trim_end(),
                now_ts,
                cmd_str,
                actor,
                secs
            );
            return Ok((
                false,
                format!(
                    "eval timed out after {}s ([policy].eval_timeout_secs)",
                    secs
                ),
            ));
        };

        let passed = output.status.success();
        let stdout_s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr_s = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let combined = if !stdout_s.is_empty() && !stderr_s.is_empty() {
            format!("{} | {}", stdout_s, stderr_s)
        } else if !stdout_s.is_empty() {
            stdout_s
        } else if !stderr_s.is_empty() {
            stderr_s
        } else {
            format!("exit {}", output.status.code().unwrap_or(-1))
        };

        card.meta.eval_status = Some(if passed {
            "passed".to_string()
        } else {
            "failed".to_string()
        });
        card.meta.updated = Local::now().format("%Y-%m-%d").to_string();
        let badge = if passed { "PASS ✅" } else { "FAIL ❌" };
        card.body = format!(
            "{}\n- `{}`: Eval `{}` run by `{}` -> **{}** (`{}`).",
            card.body.trim_end(),
            now_ts,
            cmd_str,
            actor,
            badge,
            combined.lines().next().unwrap_or("")
        );
        Ok((passed, combined))
    }
}
