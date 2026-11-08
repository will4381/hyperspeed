//! Finds browser-automation processes (Playwright, Puppeteer, Cypress, WebDriver) so they can be stopped.

use super::Shared;
use serde::Serialize;
use std::collections::HashMap;
use std::thread;
use std::time::Duration;
use sysinfo::{Pid, Process, System};

const STOP_GRACE: Duration = Duration::from_secs(3);

/// Wrappers that often mention a test command without being the test itself.
const WRAPPERS: [&str; 7] = ["zsh", "bash", "sh", "fish", "fnm", "claude", "sandbox-exec"];

/// Matched against the executable path. Order matters: the Playwright cache also holds headless Chrome.
const PROGRAMS: [(&str, &str); 8] = [
    ("ms-playwright/", "PLAYWRIGHT"),
    ("chrome-headless-shell", "HEADLESS CHROME"),
    ("/puppeteer/", "PUPPETEER"),
    ("chromedriver", "CHROMEDRIVER"),
    ("geckodriver", "GECKODRIVER"),
    ("msedgedriver", "EDGEDRIVER"),
    ("Cypress", "CYPRESS"),
    ("Google Chrome for Testing", "CHROME FOR TESTING"),
];

/// Matched against the start of the command line, for runners launched through node or npm.
const RUNNERS: [(&str, &str); 8] = [
    ("playwright test", "PLAYWRIGHT"),
    ("node_modules/playwright", "PLAYWRIGHT"),
    ("/.bin/playwright", "PLAYWRIGHT"),
    ("@playwright/mcp", "PLAYWRIGHT MCP"),
    ("cypress run", "CYPRESS"),
    ("cypress open", "CYPRESS"),
    ("/.bin/wdio", "WEBDRIVERIO"),
    ("puppeteer", "PUPPETEER"),
];

#[derive(Serialize)]
pub(super) struct Browser {
    pid: u32,
    kind: &'static str,
    project: String,
    age: u64,
    orphan: bool,
    procs: u32,
    cpu: f32,
    mem: u64,
    #[serde(skip)]
    pids: Vec<i32>,
}

fn classify(name: &str, args: &[String]) -> Option<&'static str> {
    if WRAPPERS.contains(&name) {
        return None;
    }
    let program = args.first().map_or(name, String::as_str);
    if let Some((_, kind)) = PROGRAMS.iter().find(|(needle, _)| program.contains(needle)) {
        return Some(kind);
    }
    let line = args.iter().take(5).cloned().collect::<Vec<_>>().join(" ");
    RUNNERS.iter().find(|(needle, _)| line.contains(needle)).map(|(_, kind)| *kind)
}

fn project_of(p: &Process) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let Some(cwd) = p.cwd().map(|c| c.display().to_string()).filter(|c| *c != "/" && *c != home) else {
        return "-".into();
    };
    let parts: Vec<&str> = cwd.rsplit('/').take(2).collect();
    format!("{}/{}", parts[1], parts[0]).trim_start_matches('/').to_string()
}

/// One row per automation tree: a matching process whose parent doesn't match, plus everything under it.
pub(super) fn scan(sys: &System) -> Vec<Browser> {
    let mut children: HashMap<Pid, Vec<&Process>> = HashMap::new();
    let mut kinds: HashMap<Pid, &'static str> = HashMap::new();
    for p in sys.processes().values() {
        if let Some(parent) = p.parent() {
            children.entry(parent).or_default().push(p);
        }
        let args: Vec<String> = p.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        if let Some(kind) = classify(&p.name().to_string_lossy(), &args) {
            kinds.insert(p.pid(), kind);
        }
    }
    let mut rows = vec![];
    for (pid, kind) in &kinds {
        let root = &sys.processes()[pid];
        if root.parent().is_some_and(|parent| kinds.contains_key(&parent)) {
            continue;
        }
        let (mut cpu, mut mem, mut pids) = (0.0, 0, vec![]);
        let mut stack = vec![root];
        while let Some(p) = stack.pop() {
            cpu += p.cpu_usage();
            mem += p.memory();
            pids.push(p.pid().as_u32() as i32);
            stack.extend(children.get(&p.pid()).into_iter().flatten());
        }
        rows.push(Browser {
            pid: pid.as_u32(),
            kind,
            project: project_of(root),
            age: root.run_time(),
            orphan: root.parent().is_none_or(|parent| parent.as_u32() <= 1),
            procs: pids.len() as u32,
            cpu,
            mem,
            pids,
        });
    }
    rows.sort_by_key(|b| (!b.orphan, u64::MAX - b.age));
    rows
}

/// Stops one tree by root pid, or every tree. Only pids from the latest scan can be signalled.
pub(super) fn stop(s: &Shared, root: Option<u64>) -> Result<(), String> {
    let targets: Vec<i32> = {
        let d = s.lock().unwrap();
        d.browsers.iter().filter(|b| root.is_none_or(|r| r == b.pid as u64)).flat_map(|b| b.pids.iter().copied()).collect()
    };
    if root.is_some() && targets.is_empty() {
        return Err("that process already exited".into());
    }
    for pid in &targets {
        unsafe { libc::kill(*pid, libc::SIGTERM) };
    }
    thread::spawn(move || {
        thread::sleep(STOP_GRACE);
        for pid in targets {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn classify_finds_automation_but_not_wrappers_or_real_chrome() {
        let shell = "/Users/x/Library/Caches/ms-playwright/chromium-1/chrome-headless-shell --headless";
        assert_eq!(classify("chrome-headless-shell", &args(shell)), Some("PLAYWRIGHT"));
        assert_eq!(classify("node", &args("node /app/node_modules/.bin/playwright test")), Some("PLAYWRIGHT"));
        assert_eq!(classify("chromedriver", &args("/usr/local/bin/chromedriver --port=1")), Some("CHROMEDRIVER"));
        assert_eq!(classify("zsh", &args("zsh -c npx playwright test")), None);
        assert_eq!(classify("Google Chrome", &args("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")), None);
        assert_eq!(classify("node", &args("node server.js")), None);
    }
}
