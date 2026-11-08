//! Cloudflare tunnels for registered servers.

use super::jobs::save;
use super::logs::log;
use super::{Shared, Tunnel};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

pub(super) fn find_binary(name: &str, path: &str) -> Option<PathBuf> {
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

pub(super) fn tunnel_up(s: &Shared, name: &str, host: Option<Option<String>>) -> Result<(), String> {
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

pub(super) fn tunnel_down(s: &Shared, name: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_url_ignores_api_host() {
        let line = "2026 INF |  https://calm-river-12.trycloudflare.com                  |";
        assert_eq!(parse_quick_url(line).as_deref(), Some("https://calm-river-12.trycloudflare.com"));
        assert_eq!(parse_quick_url("failed to reach https://api.trycloudflare.com/tunnel"), None);
    }
}
