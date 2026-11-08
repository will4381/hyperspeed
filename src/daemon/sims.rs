//! Simulator control through simctl.

use super::logs::{log, pump};
use super::{Daemon, Shared, Sim, SimOp};
use serde_json::Value;
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::{Pid, Process, System};

const SIM_TIMEOUT: Duration = Duration::from_secs(180);

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
            sims.push(Sim { udid: field("udid"), name: field("name"), runtime: short.clone(), state: field("state"), size: d["dataPathSize"].as_u64().unwrap_or(0) });
        }
    }
    sims.sort_by(|a, b| (a.state == "Shutdown").cmp(&(b.state == "Shutdown")).then(b.runtime.cmp(&a.runtime)).then(a.name.cmp(&b.name)));
    Some(sims)
}

pub(super) fn refresh_sims(s: &Shared) {
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

pub(super) fn sim_action(s: &Shared, reference: &str, action: &str) -> Result<(), String> {
    let mut d = s.lock().unwrap();
    let sim = find_sim(&d, reference)?;
    // Names repeat across runtimes, and find_sim's best guess isn't good enough for something permanent.
    if action == "delete" && sim.udid != reference && d.sims.iter().filter(|x| x.name.eq_ignore_ascii_case(reference)).count() > 1 {
        return Err(format!("several simulators are named {}: use the UDID", sim.name));
    }
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
        "boot" | "shutdown" | "restart" | "delete" => {}
        _ => return Err(format!("unknown simulator action: {action}")),
    }
    if let Some(op) = d.sim_busy.get_mut(&sim.udid) {
        if (action == "shutdown" && op.label == "stopping") || (action == "delete" && op.label == "deleting") {
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
        ("delete", "Shutdown") => &["delete"],
        ("delete", _) => return Err(format!("{} must be shut down first", sim.name)),
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
        "delete" => ("deleting", None),
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
        // Refresh before clearing busy, so a finished op never shows as idle with its old state.
        refresh_sims(&s);
        let mut d = s.lock().unwrap();
        if d.sim_busy.get(&sim.udid).is_some_and(|op| op.id == id) {
            d.sim_busy.remove(&sim.udid);
        }
    });
    Ok(())
}

/// CPU, memory and process count of every process under each simulator's launchd_sim, by UDID.
pub(super) fn simulator_usage(sys: &System) -> HashMap<String, (f32, u64, u32)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simctl_parses_sorts_and_skips_unavailable() {
        let json = r#"{"devices":{
            "com.apple.CoreSimulator.SimRuntime.iOS-18-3":[{"udid":"A","name":"Old","state":"Shutdown","isAvailable":true}],
            "com.apple.CoreSimulator.SimRuntime.iOS-27-0":[
                {"udid":"B","name":"New","state":"Shutdown","isAvailable":true},
                {"udid":"C","name":"Gone","state":"Shutdown","isAvailable":false},
                {"udid":"D","name":"Live","state":"Booted","isAvailable":true,"dataPathSize":42}]}}"#;
        let sims = parse_simctl(json).unwrap();
        assert_eq!(sims.iter().map(|s| s.udid.as_str()).collect::<Vec<_>>(), ["D", "B", "A"]);
        assert_eq!(sims[0].runtime, "iOS 27.0");
        assert_eq!((sims[0].size, sims[1].size), (42, 0));
    }
}
