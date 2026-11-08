mod daemon;
mod window;

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::{net::UnixStream, process::CommandExt};
use std::process::{Command, Stdio};
use std::time::Duration;

const USAGE: &str = "hyperspeed: run all your dev servers from one place

  hyperspeed                                open the dashboard
  hyperspeed add <name> [opts] -- <cmd...>  register a job or server
      --port N      port it serves on (auto-detected if omitted, exported as $PORT)
      --cwd DIR     working directory (default: current)
      --health /p   HTTP path to probe for response time
      --env K=V     extra environment variable (repeatable)
  hyperspeed rm <name>
  hyperspeed ls
  hyperspeed start|stop|restart <name>|--all
  hyperspeed logs [name] [-f] [-n 100]
  hyperspeed tunnel <name> [--host dev.example.com | --quick]
  hyperspeed untunnel <name>
  hyperspeed sim ls
  hyperspeed sim boot|stop|restart|open|logs|delete <name or udid>
  hyperspeed quit                           stop everything and shut down the daemon
";

/// One request/response round trip with the daemon. Also used by the window.
pub fn call(req: &str) -> Result<String, String> {
    let mut stream = UnixStream::connect(daemon::socket_path()).map_err(|e| e.to_string())?;
    stream.write_all(req.trim_end().as_bytes()).map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line)
}

pub fn ensure_daemon() -> Result<(), String> {
    if UnixStream::connect(daemon::socket_path()).is_ok() {
        return Ok(());
    }
    std::fs::create_dir_all(daemon::data_dir()).map_err(|e| e.to_string())?;
    let log = std::fs::File::create(daemon::data_dir().join("daemon.log")).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    cmd.arg("daemon").current_dir(daemon::data_dir()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    // Own session, so closing the terminal that launched it leaves the servers running.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        })
    };
    cmd.spawn().map_err(|e| format!("couldn't start daemon: {e}"))?;
    for _ in 0..100 {
        if UnixStream::connect(daemon::socket_path()).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("daemon didn't come up; see ~/Library/Application Support/Hyperspeed/daemon.log".into())
}

fn rpc(req: Value) -> Result<Value, String> {
    let resp: Value = serde_json::from_str(&call(&req.to_string())?).map_err(|e| e.to_string())?;
    match resp["ok"].as_bool() {
        Some(true) => Ok(resp),
        _ => Err(resp["error"].as_str().unwrap_or("daemon error").to_string()),
    }
}

/// Parses `add` arguments into a job. `cwd` and `path` are the caller's environment.
fn parse_add(args: &[String], cwd: &str, path: &str) -> Result<Value, String> {
    let split = args.iter().position(|a| a == "--").ok_or("missing `-- <command>`")?;
    let (opts, cmd) = (&args[..split], &args[split + 1..]);
    if cmd.is_empty() {
        return Err("missing command after --".into());
    }
    let mut it = opts.iter();
    let name = it.next().filter(|n| !n.starts_with("--")).ok_or("missing job name")?;
    let (mut port, mut job_cwd, mut health) = (None, cwd.to_string(), None);
    let mut env = BTreeMap::new();
    while let Some(flag) = it.next() {
        let val = it.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--port" => port = Some(val.parse::<u16>().map_err(|_| format!("bad port: {val}"))?),
            "--cwd" => job_cwd = std::fs::canonicalize(val).map_err(|e| format!("{val}: {e}"))?.display().to_string(),
            "--health" => health = Some(val.clone()),
            "--env" => {
                let (k, v) = val.split_once('=').ok_or("--env needs K=V")?;
                env.insert(k.to_string(), v.to_string());
            }
            _ => return Err(format!("unknown option: {flag}")),
        }
    }
    Ok(json!({
        "name": name, "cmd": cmd.join(" "), "cwd": job_cwd, "port": port, "health": health,
        "path": path, "env": env,
    }))
}

fn print_logs(lines: &Value) -> u64 {
    let mut last = 0;
    for l in lines.as_array().into_iter().flatten() {
        println!("{:<12} {}", l["job"].as_str().unwrap_or_default(), l["text"].as_str().unwrap_or_default());
        last = l["seq"].as_u64().unwrap_or(last);
    }
    last
}

fn job_names(args: &[String]) -> Result<Vec<String>, String> {
    match args.first().map(String::as_str) {
        None => Err("name a job or pass --all".into()),
        Some("--all") => {
            let state = rpc(json!({ "op": "state" }))?;
            Ok(state["jobs"].as_array().into_iter().flatten().filter_map(|j| j["name"].as_str().map(String::from)).collect())
        }
        Some(name) => Ok(vec![name.to_string()]),
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => ("open", &[][..]),
    };
    match cmd {
        "daemon" => return daemon::run(),
        "help" | "-h" | "--help" => {
            print!("{USAGE}");
            return Ok(());
        }
        _ => {}
    }
    ensure_daemon()?;
    let name = rest.first().map(String::as_str).unwrap_or_default();
    match cmd {
        "open" => {
            // From the installed CLI, go through the app so the Dock shows its name and icon.
            let in_bundle = std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"));
            let installed = std::path::Path::new("/Applications/Hyperspeed.app").exists();
            if !in_bundle && installed && !cfg!(debug_assertions) {
                Command::new("/usr/bin/open").args(["-a", "/Applications/Hyperspeed.app"]).status().map_err(|e| e.to_string())?;
            } else {
                window::open()?;
            }
        }
        "add" => {
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?.display().to_string();
            let path = std::env::var("PATH").unwrap_or_default();
            let job = parse_add(rest, &cwd, &path)?;
            let resp = rpc(json!({ "op": "add", "job": job }))?;
            println!("{} {}", if resp["updated"] == true { "updated" } else { "added" }, job["name"].as_str().unwrap_or_default());
        }
        "rm" | "untunnel" => {
            rpc(json!({ "op": cmd, "name": name }))?;
        }
        "start" | "stop" | "restart" => {
            for job in job_names(rest)? {
                match rpc(json!({ "op": cmd, "name": job })) {
                    Ok(_) => println!("{cmd}: {job}"),
                    Err(e) => eprintln!("{e}"),
                }
            }
        }
        "ls" => {
            let state = rpc(json!({ "op": "state" }))?;
            println!("{:<20} {:<9} {:<7} {:<8} {:<9} COMMAND", "NAME", "STATE", "PORT", "PID", "MEM");
            for j in state["jobs"].as_array().into_iter().flatten() {
                let port = j["port"].as_u64().or(j["ports"][0].as_u64()).map_or("-".into(), |p| p.to_string());
                let pid = j["pid"].as_u64().map_or("-".into(), |p| p.to_string());
                let mem = format!("{}MB", j["mem"].as_u64().unwrap_or(0) / 1_048_576);
                println!("{:<20} {:<9} {:<7} {:<8} {:<9} {}", j["name"].as_str().unwrap_or(""), j["status"].as_str().unwrap_or(""), port, pid, mem, j["cmd"].as_str().unwrap_or(""));
            }
        }
        "logs" => {
            let follow = rest.iter().any(|a| a == "-f");
            let limit = rest.iter().position(|a| a == "-n").and_then(|i| rest.get(i + 1)).and_then(|n| n.parse::<u64>().ok()).unwrap_or(100);
            let job = rest.iter().enumerate().find(|(i, a)| !a.starts_with('-') && (*i == 0 || rest[i - 1] != "-n")).map_or("", |(_, a)| a.as_str());
            let mut after = 0;
            let mut req = json!({ "op": "logs", "name": job, "after": after, "limit": limit });
            loop {
                let resp = rpc(req.clone())?;
                after = print_logs(&resp["lines"]).max(after);
                if !follow {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
                req = json!({ "op": "logs", "name": job, "after": after });
            }
        }
        "tunnel" => {
            let mut req = json!({ "op": "tunnel", "name": name });
            if rest.iter().any(|a| a == "--quick") {
                req["host"] = Value::Null;
            }
            if let Some(h) = rest.iter().position(|a| a == "--host").and_then(|i| rest.get(i + 1)) {
                req["host"] = json!(h);
            }
            rpc(req)?;
            for _ in 0..120 {
                let state = rpc(json!({ "op": "state" }))?;
                let job = state["jobs"].as_array().into_iter().flatten().find(|j| j["name"] == name).cloned().unwrap_or_default();
                match job["tunnel"]["status"].as_str() {
                    Some("live") => {
                        println!("{}", job["tunnel"]["url"].as_str().unwrap_or_default());
                        return Ok(());
                    }
                    Some("failed") => return Err(format!("tunnel failed: hyperspeed logs {name}")),
                    _ => std::thread::sleep(Duration::from_millis(250)),
                }
            }
            return Err("tunnel still starting: check the dashboard".into());
        }
        "sim" => {
            let target = rest.get(1).map(String::as_str).unwrap_or_default();
            // A freshly started daemon needs a moment for its first simctl listing.
            for _ in 0..100 {
                if !rpc(json!({ "op": "state" }))?["simctl"].is_null() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            match name {
                "ls" | "" => {
                    let state = rpc(json!({ "op": "state" }))?;
                    if state["simctl"] != true {
                        return Err("simulator list unavailable: is Xcode installed?".into());
                    }
                    println!("{:<30} {:<14} {:<14} {:<9} UDID", "NAME", "RUNTIME", "STATE", "DISK");
                    for x in state["sims"].as_array().into_iter().flatten() {
                        let disk = format!("{}MB", x["size"].as_u64().unwrap_or(0) / 1_048_576);
                        println!("{:<30} {:<14} {:<14} {:<9} {}", x["name"].as_str().unwrap_or(""), x["runtime"].as_str().unwrap_or(""), x["state"].as_str().unwrap_or(""), disk, x["udid"].as_str().unwrap_or(""));
                    }
                }
                action => {
                    let action = if action == "stop" { "shutdown" } else { action };
                    rpc(json!({ "op": "sim", "action": action, "name": target }))?;
                }
            }
        }
        "quit" => {
            rpc(json!({ "op": "quit" }))?;
        }
        _ => return Err(format!("unknown command: {cmd}\n\n{USAGE}")),
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("hyperspeed: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parse_add_reads_flags_and_command() {
        let job = parse_add(&args("web --port 3000 --health /up --env A=b=c -- npm run dev"), "/x", "/bin").unwrap();
        assert_eq!(job["name"], "web");
        assert_eq!(job["port"], 3000);
        assert_eq!(job["cmd"], "npm run dev");
        assert_eq!(job["cwd"], "/x");
        assert_eq!(job["env"]["A"], "b=c");
    }

    #[test]
    fn parse_add_rejects_bad_input() {
        assert!(parse_add(&args("web npm run dev"), "/x", "").is_err());
        assert!(parse_add(&args("web --"), "/x", "").is_err());
        assert!(parse_add(&args("web --port nope -- x"), "/x", "").is_err());
    }
}
