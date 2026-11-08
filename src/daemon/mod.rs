//! Background supervisor: owns the job registry, child processes, metrics, logs and tunnels.
//! Speaks newline-delimited JSON on a 0600 Unix socket, so nothing is exposed on TCP.

mod browsers;
mod jobs;
mod logs;
mod metrics;
mod sims;
mod tunnel;

use jobs::{add, remove, start, stop};
use metrics::sampler;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sims::{refresh_sims, sim_action};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tunnel::{tunnel_down, tunnel_up};

const STOP_GRACE: Duration = Duration::from_secs(5);

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
    /// Epoch milliseconds of the most recent start, kept across daemon restarts.
    #[serde(default)]
    pub last_started: Option<u64>,
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
    size: u64,
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
    browsers: Vec<browsers::Browser>,
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
                "last_started": j.last_started, "pid": r.pgid, "uptime": r.started.map(|t| t.elapsed().as_secs()),
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
                "udid": x.udid, "name": x.name, "runtime": x.runtime, "state": x.state, "size": x.size,
                "busy": d.sim_busy.get(&x.udid).map(|op| op.label), "logging": d.sim_logs.contains_key(&x.udid),
                "cpu": cpu, "mem": mem, "procs": procs,
            })
        })
        .collect();
    json!({ "host": d.host, "jobs": jobs, "sims": sims, "browsers": d.browsers, "simctl": d.simctl, "seq": d.seq })
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
        "browser_stop" => done(browsers::stop(s, req["pid"].as_u64())),
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
        browsers: vec![],
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
