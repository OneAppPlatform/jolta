//! Download with an animated progress bar (spinner + bar + MB + throughput)
//! when stderr is a terminal; plain single-line logging otherwise.
//! Shells out to curl — no HTTP stack in the binary.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::ui::{die, fmt_bytes, paint, tty};

/// Content length of the final target of `url`, if the server reports one.
/// (Last Content-Length header across the redirect chain wins.)
fn content_length(url: &str) -> Option<u64> {
    let o = Command::new("curl").args(["-sIL"]).arg(url).output().ok()?;
    let headers = String::from_utf8_lossy(&o.stdout);
    let mut len = None;
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                if let Ok(v) = value.trim().parse::<u64>() {
                    len = Some(v);
                }
            }
        }
    }
    len
}

pub fn download(url: &str, dest: &Path, label: &str) -> bool {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const BAR: usize = 24;

    let total = content_length(url).filter(|t| *t > 0);
    let mut child = match Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(dest)
        .arg(url)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} cannot run curl: {e}", paint("31", "jolta:", true));
            return false;
        }
    };

    let started = Instant::now();
    let fancy = tty(true);
    if !fancy {
        eprintln!(
            "jolta: downloading {label}{}",
            total.map(|t| format!(" ({})", fmt_bytes(t))).unwrap_or_default()
        );
    }
    let mut frame = 0usize;
    let mut last_bytes = 0u64;
    let mut last_t = started;
    let mut speed = 0f64;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => die(&format!("curl: {e}")),
        }
        if fancy {
            let bytes = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
            let now = Instant::now();
            let dt = now.duration_since(last_t).as_secs_f64();
            if dt > 0.5 {
                speed = (bytes.saturating_sub(last_bytes)) as f64 / dt;
                last_bytes = bytes;
                last_t = now;
            }
            let (bar, pct) = match total {
                Some(t) => {
                    let done = ((bytes as f64 / t as f64) * BAR as f64) as usize;
                    let done = done.min(BAR);
                    (
                        format!("{}{}", "█".repeat(done), "░".repeat(BAR - done)),
                        format!("{:>3.0}%", bytes as f64 / t as f64 * 100.0),
                    )
                }
                None => ("░".repeat(BAR), "  ?%".into()),
            };
            let speed_s = if speed > 0.0 {
                format!("  {}/s", fmt_bytes(speed as u64))
            } else {
                String::new()
            };
            eprint!(
                "\r\x1b[2K  \x1b[36m{}\x1b[0m {label}  \x1b[36m{bar}\x1b[0m {pct}  {}{}\x1b[2m{speed_s}\x1b[0m ",
                FRAMES[frame % FRAMES.len()],
                fmt_bytes(bytes),
                total.map(|t| format!(" / {}", fmt_bytes(t))).unwrap_or_default()
            );
            let _ = io::stderr().flush();
            frame += 1;
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let bytes = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    if fancy {
        eprint!("\r\x1b[2K");
    }
    if status.success() {
        eprintln!(
            "  {} downloaded {label}  {}",
            paint("32", "✓", true),
            paint(
                "2",
                &format!("{} in {:.1}s", fmt_bytes(bytes), started.elapsed().as_secs_f64()),
                true
            )
        );
        true
    } else {
        eprintln!("  {} download failed for {label}", paint("31", "✗", true));
        false
    }
}
