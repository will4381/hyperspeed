//! Background supervisor: owns the job registry, child processes, metrics, logs and tunnels.
//! Speaks newline-delimited JSON on a 0600 Unix socket, so nothing is exposed on TCP.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
    process::CommandExt,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

// ponytail: one shared ring, so a very noisy job can evict a quiet job's history. Per-job rings if that bites.
const LOG_CAP: usize = 20_000;
const HIST_LEN: usize = 120;
const STOP_GRACE: Duration = Duration::from_secs(5);
const SIM_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub name: String,
    pub cmd: String,
    pub cwd: String,
    pub port: Option<u16>,
    pub health: Option<String>,
    /// PATH captured at `add`, so nvm/pyenv/etc resolve the same as in the user's terminal.
    pub path: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Custom hostname for a named Cloudflare tunnel. None means a quick trycloudflare URL.
    #[serde(default)]
    pub host: Option<String>,
}

#[derive(Default)]
struct Tunnel {
    status: &'static str, // "" (off), starting, live, failed
    url: Option<String>,
    pid: Option<i32>,
    stopping: bool,
}

#[derive(Default)]
struct Hist {
    cpu: VecDeque<f32>,
    mem: VecDeque<f32>,
    rate: VecDeque<f32>,
    latency: VecDeque<f32>,
}

#[derive(Default)]
struct Run {
    status: &'static str, // "" (never started), running, stopping, stopped, exited, crashed
    pgid: Option<i32>,
    started: Option<Instant>,
    exit_code: Option<i32>,
    restarts: u32,
    stop_requested: bool,
    restart_on_exit: bool,
    cpu: f32,
    mem: u64,
    procs: u32,
    io: u64,
    ports: Vec<u16>,
    latency: Option<f32>,
    http: Option<u16>,
    lines: u64,
    warns: u64,
    errors: u64,
    lines_tick: u32,
    hist: Hist,
    tunnel: Tunnel,
}

#[derive(Serialize, Clone)]
struct Line {
    seq: u64,
    ts: u64,
    job: String,
    stream: &'static str,
    level: &'static str,
    text: String,
}

#[derive(Clone)]
struct Sim {
    udid: String,
    name: String,
    runtime: String,
    state: String,
}

/// A boot/shutdown in flight. `target` lets the real device state end it early, since simctl can hang after the work is done.
struct SimOp {
    id: u64,
    label: &'static str,
    target: Option<&'static str>,
    pid: Option<i32>,
    cancelled: bool,
}

struct Daemon {
    jobs: BTreeMap<String, (Job, Run)>,
    sims: Vec<Sim>,
    simctl: Option<bool>, // None until the first listing finishes
    sim_usage: HashMap<String, (f32, u64, u32)>,
    sim_busy: HashMap<String, SimOp>,
    sim_op_id: u64,
    sim_logs: HashMap<String, i32>,
    logs: VecDeque<Line>,
    seq: u64,
    host: Value,
}

type Shared = Arc<Mutex<Daemon>>;

pub fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home).join("Library/Application Support/Hyperspeed")
}

pub fn socket_path() -> PathBuf {
    data_dir().join("daemon.sock")
}

fn registry_path() -> PathBuf {
    data_dir().join("jobs.json")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn push_hist(h: &mut VecDeque<f32>, v: f32) {
    if h.len() == HIST_LEN {
        h.pop_front();
    }
    h.push_back(v);
}

/// Removes ANSI escapes and keeps only what a `\r` progress line ended up showing.
fn clean_line(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' if chars.peek().is_some() => out.clear(),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Whole-token match, so "0 errors" and "errorHandler" don't count as errors.
fn classify(text: &str) -> &'static str {
    let lower = text.to_ascii_lowercase();
    let has = |words: &[&str]| lower.split(|c: char| !c.is_ascii_alphanumeric()).any(|t| words.contains(&t));
    let error_type = lower.split(|c: char| !c.is_ascii_alphanumeric()).any(|t| t.ends_with("error")); // TypeError, etc
    if error_type || has(&["err", "fatal", "panic", "exception", "uncaught", "traceback", "failed"]) {
        "err"
    } else if has(&["warn", "warning", "wrn", "deprecated"]) {
        "warn"
    } else {
        "info"
    }
}

fn log(d: &mut Daemon, job: &str, stream: &'static str, raw: &str) {
    let text: String = clean_line(raw).chars().take(8192).collect();
    let level = classify(&text);
    if let Some((_, run)) = d.jobs.get_mut(job) {
        if stream == "out" || stream == "err" {
            run.lines += 1;
            run.lines_tick += 1;
            match level {
                "err" => run.errors += 1,
                "warn" => run.warns += 1,
                _ => {}
            }
        }
    }
    d.seq += 1;
    let line = Line { seq: d.seq, ts: now_ms(), job: job.into(), stream, level, text };
    d.logs.push_back(line);
    if d.logs.len() > LOG_CAP {
        d.logs.pop_front();
    }
}

fn save(d: &Daemon) {
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

fn pump(s: &Shared, job: &str, stream: &'static str, reader: impl Read + Send + 'static) {
    let (s, job) = (s.clone(), job.to_string());
    thread::spawn(move || {
        for line in BufReader::new(reader).split(b'\n').map_while(Result::ok) {
            let text = String::from_utf8_lossy(&line);
            log(&mut s.lock().unwrap(), &job, stream, &text);
        }
    });
}

fn start(s: &Shared, name: &str) -> Result<(), String> {
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

fn stop(s: &Shared, name: &str, restart: bool) -> Result<(), String> {
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

fn find_binary(name: &str, path: &str) -> Option<PathBuf> {
    format!("{path}:/opt/homebrew/bin:/usr/local/bin")
        .split(':')
        .map(|dir| Path::new(dir).join(name))
        .find(|p| p.is_file())
}

/// The public URL cloudflared prints for a quick tunnel; ignores its API host in error text.
fn parse_quick_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url = line[start..].split_whitespace().next()?.trim_end_matches(['|', '"']);
    (url.ends_with(".trycloudflare.com") && !url.contains("//api.")).then(|| url.to_string())
}

fn valid_host(h: &str) -> bool {
    !h.is_empty() && h.contains('.') && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') && !h.starts_with('-')
}

fn tunnel_up(s: &Shared, name: &str, host: Option<Option<String>>) -> Result<(), String> {
    if let Some(Some(h)) = &host {
        if !valid_host(h) {
            return Err(format!("invalid hostname: {h}"));
        }
    }
    let mut d = s.lock().unwrap();
    let (job, run) = d.jobs.get_mut(name).ok_or(format!("no job named {name}"))?;
    if matches!(run.tunnel.status, "starting" | "live") {
        return Err(format!("{name} already has a tunnel"));
    }
    let port = job.port.or(run.ports.first().copied()).ok_or(format!("{name} has no port yet: start it or register it with --port"))?;
    let bin = find_binary("cloudflared", &job.path).ok_or("cloudflared not found: brew install cloudflared")?;
    if let Some(h) = host {
        job.host = h;
    }
    let (host, name) = (job.host.clone(), name.to_string());
    run.tunnel = Tunnel { status: "starting", ..Default::default() };
    save(&d);
    drop(d);

    let s = s.clone();
    thread::spawn(move || {
        let tunnel_name = format!("hyperspeed-{name}");
        if let Some(h) = &host {
            // Both are no-ops or harmless failures once set up. Never overwrites an existing DNS record.
            for args in [vec!["tunnel", "create", &tunnel_name], vec!["tunnel", "route", "dns", &tunnel_name, h]] {
                if let Ok(out) = Command::new(&bin).args(&args).output() {
                    let text = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
                    let mut d = s.lock().unwrap();
                    text.lines().for_each(|l| log(&mut d, &name, "tunnel", l));
                }
            }
        }
        let url = format!("http://localhost:{port}");
        // Rewriting Host keeps dev servers with host allow-lists (Vite) from rejecting the public name.
        let host_header = format!("localhost:{port}");
        let mut cmd = Command::new(&bin);
        cmd.args(["tunnel", "--no-autoupdate", "--url", &url, "--http-host-header", &host_header]);
        if host.is_some() {
            cmd.args(["run", &tunnel_name]);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let mut d = s.lock().unwrap();
                log(&mut d, &name, "sys", &format!("error: cloudflared failed to start: {e}"));
                if let Some((_, r)) = d.jobs.get_mut(&name) {
                    r.tunnel.status = "failed";
                }
                return;
            }
        };
        {
            let mut d = s.lock().unwrap();
            if let Some((_, r)) = d.jobs.get_mut(&name) {
                r.tunnel.pid = Some(child.id() as i32);
                if r.tunnel.stopping {
                    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
                }
            }
        }
        let (out, err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        for reader in [Box::new(out) as Box<dyn Read + Send>, Box::new(err)] {
            let (s, name, host) = (s.clone(), name.clone(), host.clone());
            thread::spawn(move || {
                for line in BufReader::new(reader).lines().map_while(Result::ok) {
                    let mut d = s.lock().unwrap();
                    log(&mut d, &name, "tunnel", &line);
                    let Some((_, r)) = d.jobs.get_mut(&name) else { return };
                    let live_url = match &host {
                        Some(h) => line.contains("Registered tunnel connection").then(|| format!("https://{h}")),
                        None => parse_quick_url(&line),
                    };
                    if let Some(u) = live_url {
                        r.tunnel.status = "live";
                        r.tunnel.url = Some(u);
                    }
                }
            });
        }
        let _ = child.wait();
        let mut d = s.lock().unwrap();
        if let Some((_, r)) = d.jobs.get_mut(&name) {
            let expected = r.tunnel.stopping;
            r.tunnel = Tunnel { status: if expected { "" } else { "failed" }, ..Default::default() };
            log(&mut d, &name, "sys", if expected { "tunnel closed" } else { "error: tunnel exited unexpectedly" });
        }
    });
    Ok(())
}

fn tunnel_down(s: &Shared, name: &str) {
    let mut d = s.lock().unwrap();
    let Some((_, r)) = d.jobs.get_mut(name) else { return };
    match r.tunnel.pid {
        Some(pid) => {
            r.tunnel.stopping = true;
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        None if r.tunnel.status == "starting" => r.tunnel.stopping = true, // the spawner sees this and closes it
        None => r.tunnel = Tunnel::default(),
    }
}

/// `lsof -F pgn` output to listening ports by process group.
fn parse_lsof(out: &str) -> HashMap<i32, Vec<u16>> {
    let mut map: HashMap<i32, Vec<u16>> = HashMap::new();
    let mut pgid = 0;
    for line in out.lines() {
        match line.split_at_checked(1) {
            Some(("g", v)) => pgid = v.parse().unwrap_or(0),
            Some(("n", v)) => {
                if let Some(port) = v.rsplit(':').next().and_then(|p| p.parse().ok()) {
                    let ports = map.entry(pgid).or_default();
                    if !ports.contains(&port) {
                        ports.push(port);
                        ports.sort();
                    }
                }
            }
            _ => {}
        }
    }
    map
}

/// GET against the job's health path: (status, milliseconds).
fn http_probe(port: u16, path: &str) -> Option<(u16, f32)> {
    let timeout = Duration::from_secs(3);
    let t0 = Instant::now();
    let mut stream = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n").ok()?;
    let mut buf = [0u8; 32];
    let n = stream.read(&mut buf).ok()?;
    let status = std::str::from_utf8(&buf[..n]).ok()?.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, t0.elapsed().as_secs_f32() * 1000.0))
}

/// `simctl boot` can block forever, so every call is bounded and a hung device can't stay busy.
fn simctl(args: &[&str], on_spawn: impl FnOnce(i32)) -> std::io::Result<std::process::Output> {
    let mut child = Command::new("/usr/bin/xcrun").arg("simctl").args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    on_spawn(child.id() as i32);
    let deadline = Instant::now() + SIM_TIMEOUT;
    while child.try_wait()?.is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "simctl timed out after 180s"));
        }
        thread::sleep(Duration::from_millis(200));
    }
    child.wait_with_output()
}

fn parse_simctl(json: &str) -> Option<Vec<Sim>> {
    let v: Value = serde_json::from_str(json).ok()?;
    let mut sims = vec![];
    for (runtime, devices) in v["devices"].as_object()? {
        // com.apple.CoreSimulator.SimRuntime.iOS-27-0 -> iOS 27.0
        let short = runtime.rsplit('.').next().unwrap_or(runtime).replacen('-', " ", 1).replace('-', ".");
        for d in devices.as_array()?.iter().filter(|d| d["isAvailable"] == true) {
            let field = |k: &str| d[k].as_str().unwrap_or_default().to_string();
            sims.push(Sim { udid: field("udid"), name: field("name"), runtime: short.clone(), state: field("state") });
        }
    }
    sims.sort_by(|a, b| (a.state == "Shutdown").cmp(&(b.state == "Shutdown")).then(b.runtime.cmp(&a.runtime)).then(a.name.cmp(&b.name)));
    Some(sims)
}

fn refresh_sims(s: &Shared) {
    let parsed = simctl(&["list", "devices", "available", "--json"], |_| {}).ok().filter(|o| o.status.success()).and_then(|o| parse_simctl(&String::from_utf8_lossy(&o.stdout)));
    let mut d = s.lock().unwrap();
    d.simctl = Some(parsed.is_some());
    d.sims = parsed.unwrap_or_default();
    let sims = std::mem::take(&mut d.sims);
    d.sim_busy.retain(|udid, op| !sims.iter().any(|x| x.udid == *udid && Some(x.state.as_str()) == op.target));
    d.sims = sims;
}

/// By UDID, else by name. Sort order makes a shared name resolve to the booted or newest device.
fn find_sim(d: &Daemon, reference: &str) -> Result<Sim, String> {
    let by_name = || d.sims.iter().find(|s| s.name.eq_ignore_ascii_case(reference));
    d.sims.iter().find(|s| s.udid == reference).or_else(by_name).cloned().ok_or(format!("no simulator named {reference}"))
}

fn sim_action(s: &Shared, reference: &str, action: &str) -> Result<(), String> {
    let mut d = s.lock().unwrap();
    let sim = find_sim(&d, reference)?;
    let label = format!("sim:{}", sim.name);
    match action {
        "open" => {
            drop(d);
            Command::new("/usr/bin/open").args(["-a", "Simulator", "--args", "-CurrentDeviceUDID", &sim.udid]).status().map_err(|e| e.to_string())?;
            return Ok(());
        }
        "logs" => {
            if let Some(pid) = d.sim_logs.get(&sim.udid) {
                unsafe { libc::kill(*pid, libc::SIGTERM) };
                return Ok(());
            }
            if sim.state != "Booted" {
                return Err(format!("{} isn't booted", sim.name));
            }
            // Installed apps only: the whole simulator's unified log would flood the shared ring.
            let mut child = Command::new("/usr/bin/xcrun")
                .args(["simctl", "spawn", &sim.udid, "log", "stream", "--style", "compact", "--predicate", "processImagePath CONTAINS \"/Containers/Bundle/Application/\""])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())?;
            d.sim_logs.insert(sim.udid.clone(), child.id() as i32);
            log(&mut d, &label, "sys", "streaming logs from installed apps");
            pump(s, &label, "out", child.stdout.take().unwrap());
            pump(s, &label, "err", child.stderr.take().unwrap());
            drop(d);
            let s = s.clone();
            thread::spawn(move || {
                let _ = child.wait();
                let mut d = s.lock().unwrap();
                d.sim_logs.remove(&sim.udid);
                log(&mut d, &label, "sys", "log stream ended");
            });
            return Ok(());
        }
        "boot" | "shutdown" | "restart" => {}
        _ => return Err(format!("unknown simulator action: {action}")),
    }
    if let Some(op) = d.sim_busy.get_mut(&sim.udid) {
        if action == "shutdown" && op.label == "stopping" {
            return Ok(());
        }
        if action == "shutdown" && op.label == "booting" {
            op.cancelled = true;
            if let Some(pid) = op.pid {
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            return Ok(());
        }
        return Err(format!("{} is busy", sim.name));
    }
    let steps: &[&str] = match (action, sim.state.as_str()) {
        ("boot", "Booted" | "Booting") | ("shutdown", "Shutdown" | "Shutting Down") => return Ok(()),
        ("boot", _) | ("restart", "Shutdown") => &["boot"],
        ("shutdown", _) => &["shutdown"],
        _ => &["shutdown", "boot"],
    };
    d.sim_op_id += 1;
    let id = d.sim_op_id;
    let (op_label, target) = match action {
        "boot" => ("booting", Some("Booted")),
        "shutdown" => ("stopping", Some("Shutdown")),
        _ => ("booting", None), // a restart passes through both states, so only simctl can end it
    };
    d.sim_busy.insert(sim.udid.clone(), SimOp { id, label: op_label, target, pid: None, cancelled: false });
    drop(d);
    let s = s.clone();
    thread::spawn(move || {
        let track = |pid: i32| {
            if let Some(op) = s.lock().unwrap().sim_busy.get_mut(&sim.udid).filter(|op| op.id == id) {
                op.pid = Some(pid);
            }
        };
        let is_cancelled = || s.lock().unwrap().sim_busy.get(&sim.udid).is_some_and(|op| op.id == id && op.cancelled);
        for step in steps {
            let result = simctl(&[step, &sim.udid], track);
            if is_cancelled() {
                break;
            }
            let mut d = s.lock().unwrap();
            match result {
                Ok(o) if o.status.success() => log(&mut d, &label, "sys", &format!("{step} done")),
                Ok(o) => log(&mut d, &label, "sys", &format!("error: {step}: {}", String::from_utf8_lossy(&o.stderr).trim())),
                Err(e) => log(&mut d, &label, "sys", &format!("error: {step}: {e}")),
            }
        }
        if is_cancelled() {
            log(&mut s.lock().unwrap(), &label, "sys", "boot cancelled");
            refresh_sims(&s);
            // Killing simctl doesn't undo a boot the service already began.
            let started = s.lock().unwrap().sims.iter().any(|x| x.udid == sim.udid && x.state != "Shutdown");
            if started {
                if let Some(op) = s.lock().unwrap().sim_busy.get_mut(&sim.udid).filter(|op| op.id == id) {
                    op.label = "stopping";
                    op.target = Some("Shutdown");
                }
                let _ = simctl(&["shutdown", &sim.udid], |_| {});
            }
        }
        let mut d = s.lock().unwrap();
        if d.sim_busy.get(&sim.udid).is_some_and(|op| op.id == id) {
            d.sim_busy.remove(&sim.udid);
        }
        drop(d);
        refresh_sims(&s);
    });
    Ok(())
}

/// CPU, memory and process count of every process under each simulator's launchd_sim, by UDID.
fn simulator_usage(sys: &System) -> HashMap<String, (f32, u64, u32)> {
    let mut children: HashMap<Pid, Vec<&Process>> = HashMap::new();
    for p in sys.processes().values() {
        if let Some(parent) = p.parent() {
            children.entry(parent).or_default().push(p);
        }
    }
    let mut out = HashMap::new();
    for root in sys.processes().values().filter(|p| p.name() == "launchd_sim") {
        let is_udid = |c: &&str| c.len() == 36 && c.matches('-').count() == 4;
        let Some(udid) = root.cmd().iter().find_map(|a| a.to_str()?.split('/').find(is_udid).map(String::from)) else { continue };
        let total: &mut (f32, u64, u32) = out.entry(udid).or_default();
        let mut stack = vec![root];
        while let Some(p) = stack.pop() {
            total.0 += p.cpu_usage();
            total.1 += p.memory();
            total.2 += 1;
            stack.extend(children.get(&p.pid()).into_iter().flatten());
        }
    }
    out
}

fn sampler(s: Shared) {
    let mut sys = System::new();
    let cloudflared = find_binary("cloudflared", &std::env::var("PATH").unwrap_or_default()).is_some();
    let cores = System::physical_core_count().unwrap_or(0);
    let mut ports: HashMap<i32, Vec<u16>> = HashMap::new();
    let mut cpu_hist = VecDeque::new();
    let mut tick = 0u64;
    loop {
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory().with_disk_usage().with_cmd(UpdateKind::OnlyIfNotSet),
        );
        let groups: HashSet<i32> = s.lock().unwrap().jobs.values().filter_map(|(_, r)| r.pgid).collect();
        let mut usage: HashMap<i32, (f32, u64, u32, u64)> = HashMap::new();
        for (pid, p) in sys.processes() {
            let pgid = unsafe { libc::getpgid(pid.as_u32() as i32) };
            if groups.contains(&pgid) {
                let u = usage.entry(pgid).or_default();
                let disk = p.disk_usage();
                u.0 += p.cpu_usage();
                u.1 += p.memory();
                u.2 += 1;
                u.3 += disk.read_bytes + disk.written_bytes;
            }
        }
        if tick % 3 == 0 && !groups.is_empty() {
            if let Ok(out) = Command::new("/usr/sbin/lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pgn"]).output() {
                ports = parse_lsof(&String::from_utf8_lossy(&out.stdout));
            }
        }

        let mut probes = vec![];
        let mut d = s.lock().unwrap();
        for (job, run) in d.jobs.values_mut() {
            let (mut cpu, mut mem, mut procs, mut io) = (0.0, 0, 0, 0);
            if let Some(pgid) = run.pgid {
                (cpu, mem, procs, io) = usage.get(&pgid).copied().unwrap_or_default();
                run.ports = ports.get(&pgid).cloned().unwrap_or_default();
                if let (true, Some(path), Some(port)) = (tick % 5 == 0, &job.health, job.port.or(run.ports.first().copied())) {
                    probes.push((job.name.clone(), port, path.clone()));
                }
            }
            (run.cpu, run.mem, run.procs, run.io) = (cpu, mem, procs, io);
            push_hist(&mut run.hist.cpu, cpu);
            push_hist(&mut run.hist.mem, mem as f32);
            push_hist(&mut run.hist.rate, run.lines_tick as f32);
            push_hist(&mut run.hist.latency, run.latency.unwrap_or(0.0));
            run.lines_tick = 0;
        }
        d.sim_usage = simulator_usage(&sys);
        let total_cpu = sys.global_cpu_usage();
        push_hist(&mut cpu_hist, total_cpu);
        let load = System::load_average();
        d.host = json!({
            "name": System::host_name().unwrap_or_default(),
            "cpu_name": sys.cpus().first().map(|c| c.brand().to_string()).unwrap_or_default(),
            "os": System::long_os_version().unwrap_or_default(),
            "physical_cores": cores,
            "uptime": System::uptime(),
            "cpu": total_cpu,
            "cpu_hist": cpu_hist,
            "cores": sys.cpus().iter().map(|c| c.cpu_usage()).collect::<Vec<_>>(),
            "load": [load.one, load.five, load.fifteen],
            "mem_total": sys.total_memory(),
            "mem_used": sys.used_memory(),
            "mem_avail": sys.available_memory(),
            "mem_free": sys.free_memory(),
            "swap_used": sys.used_swap(),
            "swap_total": sys.total_swap(),
            "processes": sys.processes().len(),
            "cloudflared": cloudflared,
        });
        drop(d);

        for (name, port, path) in probes {
            let s = s.clone();
            thread::spawn(move || {
                let result = http_probe(port, &path);
                if let Some((_, r)) = s.lock().unwrap().jobs.get_mut(&name) {
                    r.http = result.map(|x| x.0);
                    r.latency = result.map(|x| x.1);
                }
            });
        }
        tick += 1;
        thread::sleep(Duration::from_secs(1));
    }
}

fn state(s: &Shared) -> Value {
    let d = s.lock().unwrap();
    let jobs: Vec<Value> = d
        .jobs
        .values()
        .map(|(j, r)| {
            json!({
                "name": j.name, "cmd": j.cmd, "cwd": j.cwd, "port": j.port, "health": j.health,
                "ports": r.ports,
                "status": if r.status.is_empty() { "stopped" } else { r.status },
                "pid": r.pgid, "uptime": r.started.map(|t| t.elapsed().as_secs()),
                "exit_code": r.exit_code, "restarts": r.restarts,
                "cpu": r.cpu, "mem": r.mem, "procs": r.procs, "io": r.io,
                "latency": r.latency, "http": r.http,
                "lines": r.lines, "warns": r.warns, "errors": r.errors,
                "hist": { "cpu": r.hist.cpu, "mem": r.hist.mem, "rate": r.hist.rate, "latency": r.hist.latency },
                "tunnel": { "status": r.tunnel.status, "url": r.tunnel.url, "host": j.host },
            })
        })
        .collect();
    let sims: Vec<Value> = d
        .sims
        .iter()
        .map(|x| {
            let (cpu, mem, procs) = d.sim_usage.get(&x.udid).copied().unwrap_or_default();
            json!({
                "udid": x.udid, "name": x.name, "runtime": x.runtime, "state": x.state,
                "busy": d.sim_busy.get(&x.udid).map(|op| op.label), "logging": d.sim_logs.contains_key(&x.udid),
                "cpu": cpu, "mem": mem, "procs": procs,
            })
        })
        .collect();
    json!({ "host": d.host, "jobs": jobs, "sims": sims, "simctl": d.simctl, "seq": d.seq })
}

fn add(s: &Shared, req: &Value) -> Result<Value, String> {
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
            let host = slot.0.host.take();
            slot.0 = Job { host, ..job };
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

fn remove(s: &Shared, name: &str) -> Result<(), String> {
    let mut d = s.lock().unwrap();
    let (_, run) = d.jobs.get(name).ok_or(format!("no job named {name}"))?;
    if run.pgid.is_some() {
        return Err(format!("{name} is running: stop it first"));
    }
    d.jobs.remove(name);
    save(&d);
    Ok(())
}

fn quit(s: &Shared) {
    let names: Vec<String> = s.lock().unwrap().jobs.keys().cloned().collect();
    for name in &names {
        let _ = stop(s, name, false);
        tunnel_down(s, name);
    }
    // Simulators are left booted on purpose: they may be in use outside Hyperspeed.
    for pid in s.lock().unwrap().sim_logs.values() {
        unsafe { libc::kill(*pid, libc::SIGTERM) };
    }
    let deadline = Instant::now() + STOP_GRACE + Duration::from_secs(1);
    while Instant::now() < deadline && s.lock().unwrap().jobs.values().any(|(_, r)| r.pgid.is_some() || r.tunnel.pid.is_some()) {
        thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_file(socket_path());
    std::process::exit(0);
}

fn handle(s: &Shared, req: &Value) -> Result<Value, String> {
    let name = req["name"].as_str().unwrap_or_default();
    let done = |r: Result<(), String>| r.map(|_| json!({}));
    match req["op"].as_str().unwrap_or_default() {
        "state" => Ok(state(s)),
        "logs" => {
            let after = req["after"].as_u64().unwrap_or(0);
            let limit = req["limit"].as_u64().unwrap_or(5000) as usize;
            let d = s.lock().unwrap();
            let mut lines: Vec<&Line> = d.logs.iter().rev().filter(|l| l.seq > after && (name.is_empty() || l.job == name)).take(limit).collect();
            lines.reverse();
            Ok(json!({ "lines": lines, "seq": d.seq }))
        }
        "add" => add(s, req),
        "rm" => done(remove(s, name)),
        "start" => done(start(s, name)),
        "stop" => done(stop(s, name, false)),
        "restart" => done(stop(s, name, true)),
        "tunnel" => {
            // host: absent = keep saved setting, null = quick tunnel, string = named tunnel
            let host = req.get("host").map(|h| h.as_str().map(String::from));
            done(tunnel_up(s, name, host))
        }
        "sim" => done(sim_action(s, name, req["action"].as_str().unwrap_or_default())),
        "untunnel" => {
            tunnel_down(s, name);
            Ok(json!({}))
        }
        "open_url" => {
            let url = req["url"].as_str().unwrap_or_default();
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err("only http(s) links can be opened".into());
            }
            Command::new("/usr/bin/open").arg(url).status().map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "copy" => {
            let mut child = Command::new("/usr/bin/pbcopy").stdin(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
            child.stdin.take().unwrap().write_all(req["text"].as_str().unwrap_or_default().as_bytes()).map_err(|e| e.to_string())?;
            child.wait().map_err(|e| e.to_string())?;
            Ok(json!({}))
        }
        "quit" => {
            let s = s.clone();
            thread::spawn(move || quit(&s));
            Ok(json!({}))
        }
        op => Err(format!("unknown op: {op}")),
    }
}

fn serve(s: Shared, stream: UnixStream) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let resp = match serde_json::from_str::<Value>(&line).map_err(|e| e.to_string()).and_then(|req| handle(&s, &req)) {
        Ok(Value::Object(mut map)) => {
            map.insert("ok".into(), true.into());
            Value::Object(map)
        }
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": e }),
    };
    let mut stream = stream;
    let _ = writeln!(stream, "{resp}");
}

pub fn run() -> Result<(), String> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));

    // flock releases automatically if the daemon dies, so a stale lock can't wedge startup.
    let lock = std::fs::File::create(dir.join("daemon.lock")).map_err(|e| e.to_string())?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(()); // another daemon owns it
    }
    std::mem::forget(lock);

    let _ = std::fs::remove_file(socket_path());
    let listener = UnixListener::bind(socket_path()).map_err(|e| e.to_string())?;
    std::fs::set_permissions(socket_path(), std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;

    let saved: Vec<Job> = std::fs::read(registry_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let jobs = saved.into_iter().map(|j| (j.name.clone(), (j, Run::default()))).collect();
    let shared: Shared = Arc::new(Mutex::new(Daemon {
        jobs,
        sims: vec![],
        simctl: None,
        sim_usage: HashMap::new(),
        sim_busy: HashMap::new(),
        sim_op_id: 0,
        sim_logs: HashMap::new(),
        logs: VecDeque::new(),
        seq: 0,
        host: json!({}),
    }));

    let s = shared.clone();
    thread::spawn(move || sampler(s));
    let s = shared.clone();
    thread::spawn(move || loop {
        refresh_sims(&s);
        thread::sleep(Duration::from_secs(5));
    });
    // ponytail: if the daemon itself crashes, children keep running unmanaged and the next `start` warns about the port. Persist pgids and re-adopt if that happens in practice.
    for stream in listener.incoming().map_while(Result::ok) {
        let s = shared.clone();
        thread::spawn(move || serve(s, stream));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_line_strips_escapes_and_progress() {
        assert_eq!(clean_line("\u{1b}[32mready\u{1b}[0m in 3ms"), "ready in 3ms");
        assert_eq!(clean_line("10%\r50%\rdone"), "done");
        assert_eq!(clean_line("crlf line\r"), "crlf line");
        assert_eq!(clean_line("\u{1b}]8;;http://x\u{7}link"), "link");
    }

    #[test]
    fn classify_uses_whole_tokens() {
        assert_eq!(classify("TypeError: x"), "err");
        assert_eq!(classify("errorHandler registered"), "info");
        assert_eq!(classify("Error: boom"), "err");
        assert_eq!(classify("2026-01-01 ERR connection lost"), "err");
        assert_eq!(classify("compiled with 0 errors"), "info");
        assert_eq!(classify("WARNING: slow"), "warn");
    }

    #[test]
    fn quick_url_ignores_api_host() {
        let line = "2026 INF |  https://calm-river-12.trycloudflare.com                  |";
        assert_eq!(parse_quick_url(line).as_deref(), Some("https://calm-river-12.trycloudflare.com"));
        assert_eq!(parse_quick_url("failed to reach https://api.trycloudflare.com/tunnel"), None);
    }

    #[test]
    fn simctl_parses_sorts_and_skips_unavailable() {
        let json = r#"{"devices":{
            "com.apple.CoreSimulator.SimRuntime.iOS-18-3":[{"udid":"A","name":"Old","state":"Shutdown","isAvailable":true}],
            "com.apple.CoreSimulator.SimRuntime.iOS-27-0":[
                {"udid":"B","name":"New","state":"Shutdown","isAvailable":true},
                {"udid":"C","name":"Gone","state":"Shutdown","isAvailable":false},
                {"udid":"D","name":"Live","state":"Booted","isAvailable":true}]}}"#;
        let sims = parse_simctl(json).unwrap();
        assert_eq!(sims.iter().map(|s| s.udid.as_str()).collect::<Vec<_>>(), ["D", "B", "A"]);
        assert_eq!(sims[0].runtime, "iOS 27.0");
    }

    #[test]
    fn lsof_groups_ports() {
        let out = "p100\ng90\nf20\nn*:3000\nf21\nn[::1]:3000\nf22\nn127.0.0.1:3001\np200\ng200\nf9\nn*:8080\n";
        let map = parse_lsof(out);
        assert_eq!(map[&90], vec![3000, 3001]);
        assert_eq!(map[&200], vec![8080]);
    }
}
