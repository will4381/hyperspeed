//! Captures process output into the shared log ring.

use super::{Daemon, Line, Shared};
use std::io::{BufRead, BufReader, Read};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

// ponytail: one shared ring, so a very noisy job can evict a quiet job's history. Per-job rings if that bites.
const LOG_CAP: usize = 20_000;

pub(super) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
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

pub(super) fn log(d: &mut Daemon, job: &str, stream: &'static str, raw: &str) {
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

pub(super) fn pump(s: &Shared, job: &str, stream: &'static str, reader: impl Read + Send + 'static) {
    let (s, job) = (s.clone(), job.to_string());
    thread::spawn(move || {
        for line in BufReader::new(reader).split(b'\n').map_while(Result::ok) {
            let text = String::from_utf8_lossy(&line);
            log(&mut s.lock().unwrap(), &job, stream, &text);
        }
    });
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
}
