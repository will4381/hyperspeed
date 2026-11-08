//! Periodic host, service and simulator sampling.

use super::browsers;
use super::sims::simulator_usage;
use super::tunnel::find_binary;
use super::Shared;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::{Disks, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const HIST_LEN: usize = 120;

fn push_hist(h: &mut VecDeque<f32>, v: f32) {
    if h.len() == HIST_LEN {
        h.pop_front();
    }
    h.push_back(v);
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

pub(super) fn sampler(s: Shared) {
    let mut sys = System::new();
    let cloudflared = find_binary("cloudflared", &std::env::var("PATH").unwrap_or_default()).is_some();
    let cores = System::physical_core_count().unwrap_or(0);
    let mut ports: HashMap<i32, Vec<u16>> = HashMap::new();
    let mut cpu_hist = VecDeque::new();
    let mut disks = Disks::new_with_refreshed_list();
    let mut tick = 0u64;
    loop {
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory().with_disk_usage().with_cmd(UpdateKind::OnlyIfNotSet).with_cwd(UpdateKind::OnlyIfNotSet),
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
        d.browsers = browsers::scan(&sys);
        if tick % 10 == 0 {
            disks.refresh(true);
        }
        let root = disks.list().iter().find(|d| d.mount_point() == Path::new("/"));
        let (disk_total, disk_free) = root.map_or((0, 0), |d| (d.total_space(), d.available_space()));
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
            "disk_total": disk_total,
            "disk_free": disk_free,
            "disk_used": disk_total.saturating_sub(disk_free),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsof_groups_ports() {
        let out = "p100\ng90\nf20\nn*:3000\nf21\nn[::1]:3000\nf22\nn127.0.0.1:3001\np200\ng200\nf9\nn*:8080\n";
        let map = parse_lsof(out);
        assert_eq!(map[&90], vec![3000, 3001]);
        assert_eq!(map[&200], vec![8080]);
    }
}
