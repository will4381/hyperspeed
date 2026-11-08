//! Compares the installed build with GitHub and runs the updater script.

use crate::daemon::data_dir;
use serde_json::json;
use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

const REPO: &str = "https://github.com/will4381/hyperspeed.git";
const SCRIPT: &str = include_str!("../scripts/update.sh");
/// A status this old means the updater died without reporting, such as across a reboot.
const STALE_AFTER: Duration = Duration::from_secs(30 * 60);

fn status_path() -> PathBuf {
    data_dir().join("update.status")
}

/// Set by scripts/bundle.sh. Development builds have none and never offer updates.
fn installed() -> Option<&'static str> {
    option_env!("HYPERSPEED_COMMIT").filter(|c| !c.is_empty())
}

fn is_available(installed: Option<&str>, ls_remote: &str) -> bool {
    let latest = ls_remote.split_whitespace().next().filter(|hash| hash.len() == 40);
    matches!((installed, latest), (Some(a), Some(b)) if a != b)
}

pub fn check() -> Result<String, String> {
    let out = Command::new("/usr/bin/git").args(["ls-remote", REPO, "refs/heads/main"]).env("GIT_TERMINAL_PROMPT", "0").output().map_err(|e| e.to_string())?;
    let available = out.status.success() && is_available(installed(), &String::from_utf8_lossy(&out.stdout));
    Ok(json!({ "ok": true, "available": available }).to_string())
}

fn state() -> String {
    let path = status_path();
    let state = std::fs::read_to_string(&path).unwrap_or_default().trim().to_string();
    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
    if state == "running" && age.is_some_and(|a| a > STALE_AFTER) {
        return "failed".into();
    }
    state
}

pub fn status() -> Result<String, String> {
    Ok(json!({ "ok": true, "state": state() }).to_string())
}

pub fn start() -> Result<String, String> {
    if state() == "running" {
        return Err("an update is already running".into());
    }
    std::fs::write(status_path(), "running\n").map_err(|e| e.to_string())?;
    let log = File::create(data_dir().join("update.log")).map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", SCRIPT]).env("HS_REPO", REPO).current_dir(data_dir()).stdin(Stdio::null()).stdout(log).stderr(log_err);
    // Its own session, so it outlives the window it's about to close.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        })
    };
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_is_offered_only_for_a_different_known_commit() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let output = format!("{b}\trefs/heads/main\n");
        assert!(is_available(Some(&a), &output));
        assert!(!is_available(Some(&b), &output));
        assert!(!is_available(None, &output));
        assert!(!is_available(Some(&a), ""));
    }
}
