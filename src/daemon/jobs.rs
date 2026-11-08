//! Job registry and process lifecycle.

use super::logs::{log, now_ms, pump};
use super::tunnel::tunnel_down;
use super::{registry_path, Daemon, Job, Run, Shared, STOP_GRACE};
use serde_json::{json, Value};
use std::net::TcpStream;
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub(super) fn save(d: &Daemon) {
    let jobs: Vec<&Job> = d.jobs.values().map(|(j, _)| j).collect();
    let tmp = registry_path().with_extension("tmp");
    if let Ok(body) = serde_json::to_vec_pretty(&jobs) {
        // Rename is atomic, so a crash can't leave a half-written registry.
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
            let _ = std::fs::rename(&tmp, registry_path());
        }
    }
}

pub(super) fn start(s: &Shared, name: &str) -> Result<(), String> {
    let mut d = s.lock().unwrap();
    let (job, run) = d.jobs.get_mut(name).ok_or(format!("no job named {name}"))?;
    if run.pgid.is_some() {
        return Err(format!("{name} is already running"));
    }
    let job = job.clone();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut cmd = Command::new(shell);
    cmd.args(["-c", &job.cmd])
        .current_dir(&job.cwd)
        .env("PATH", &job.path)
        .env("PYTHONUNBUFFERED", "1")
        .envs(&job.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(p) = job.port {
        cmd.env("PORT", p.to_string());
    }
    if let Some(p) = job.port {
        let in_use = TcpStream::connect_timeout(&([127, 0, 0, 1], p).into(), Duration::from_millis(100)).is_ok();
        if in_use {
            log(&mut d, name, "sys", &format!("warning: port {p} is already in use by another process"));
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            log(&mut d, name, "sys", &format!("error: failed to start: {e}"));
            d.jobs.get_mut(name).unwrap().1.status = "crashed";
            return Err(format!("failed to start {name}: {e}"));
        }
    };
    let pgid = child.id() as i32;
    pump(s, name, "out", child.stdout.take().unwrap());
    pump(s, name, "err", child.stderr.take().unwrap());
    log(&mut d, name, "sys", &format!("started pid {pgid}: {}", job.cmd));
    d.jobs.get_mut(name).unwrap().0.last_started = Some(now_ms());
    save(&d);
    let run = &mut d.jobs.get_mut(name).unwrap().1;
    run.status = "running";
    run.pgid = Some(pgid);
    run.started = Some(Instant::now());
    run.exit_code = None;
    run.stop_requested = false;
    run.restart_on_exit = false;
    drop(d);

    let (s, name) = (s.clone(), name.to_string());
    thread::spawn(move || {
        let status = child.wait();
        // Sweeps anything the shell left behind in the group; ESRCH is fine.
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
        let code = status.ok().and_then(|st| st.code());
        let mut d = s.lock().unwrap();
        let Some((_, run)) = d.jobs.get_mut(&name) else { return };
        run.pgid = None;
        run.exit_code = code;
        run.started = None;
        run.cpu = 0.0;
        run.mem = 0;
        run.procs = 0;
        run.io = 0;
        run.ports.clear();
        run.latency = None;
        run.http = None;
        let restart = run.restart_on_exit;
        run.status = match (run.stop_requested, code) {
            (true, _) => "stopped",
            (false, Some(0)) => "exited",
            _ => "crashed",
        };
        if restart {
            run.restarts += 1;
        }
        let status = run.status;
        let msg = format!("{status} (exit {})", code.map_or("signal".into(), |c| c.to_string()));
        log(&mut d, &name, "sys", &msg);
        drop(d);
        if restart {
            let _ = start(&s, &name);
        }
    });
    Ok(())
}

pub(super) fn stop(s: &Shared, name: &str, restart: bool) -> Result<(), String> {
    let mut d = s.lock().unwrap();
    let (_, run) = d.jobs.get_mut(name).ok_or(format!("no job named {name}"))?;
    let Some(pgid) = run.pgid else {
        drop(d);
        return if restart { start(s, name) } else { Ok(()) };
    };
    run.restart_on_exit = restart;
    if run.stop_requested {
        return Ok(()); // already on its way down; the grace timer is running
    }
    run.stop_requested = true;
    run.status = "stopping";
    unsafe { libc::killpg(pgid, libc::SIGTERM) };
    log(&mut d, name, "sys", "sent SIGTERM");
    drop(d);
    if !restart {
        // A public URL shouldn't outlive the server it points at. Restarts keep it.
        tunnel_down(s, name);
    }
    let (s, name) = (s.clone(), name.to_string());
    thread::spawn(move || {
        thread::sleep(STOP_GRACE);
        let mut d = s.lock().unwrap();
        let still_up = d.jobs.get(&name).is_some_and(|(_, r)| r.pgid == Some(pgid));
        if still_up {
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
            log(&mut d, &name, "sys", "did not exit in 5s, sent SIGKILL");
        }
    });
    Ok(())
}

pub(super) fn add(s: &Shared, req: &Value) -> Result<Value, String> {
    let job: Job = serde_json::from_value(req["job"].clone()).map_err(|e| e.to_string())?;
    let name_ok = !job.name.is_empty() && !job.name.starts_with('-') && job.name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if !name_ok {
        return Err("job names may only use letters, numbers, dot, dash and underscore".into());
    }
    if job.cmd.trim().is_empty() {
        return Err("command is empty".into());
    }
    if !Path::new(&job.cwd).is_dir() {
        return Err(format!("no such directory: {}", job.cwd));
    }
    if job.health.as_ref().is_some_and(|h| !h.starts_with('/') || h.contains(['\r', '\n', ' '])) {
        return Err("--health must be a path like /health".into());
    }
    let mut d = s.lock().unwrap();
    let updated = match d.jobs.get_mut(&job.name) {
        Some(slot) => {
            let (host, last_started) = (slot.0.host.take(), slot.0.last_started);
            slot.0 = Job { host, last_started, ..job };
            true
        }
        None => {
            d.jobs.insert(job.name.clone(), (job, Run::default()));
            false
        }
    };
    save(&d);
    Ok(json!({ "updated": updated }))
}

pub(super) fn remove(s: &Shared, name: &str) -> Result<(), String> {
    let mut d = s.lock().unwrap();
    let (_, run) = d.jobs.get(name).ok_or(format!("no job named {name}"))?;
    if run.pgid.is_some() {
        return Err(format!("{name} is running: stop it first"));
    }
    d.jobs.remove(name);
    d.logs.retain(|l| l.job != name);
    save(&d);
    Ok(())
}
