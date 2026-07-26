//! Terminal styling: colors, glyphs, byte formatting, fatal errors.
//! Everything degrades to plain text when the stream is not a terminal,
//! and respects NO_COLOR / TERM=dumb.

use std::env;
use std::io::{self, IsTerminal, Write};
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

pub fn tty(err: bool) -> bool {
    static OUT: OnceLock<bool> = OnceLock::new();
    static ERR: OnceLock<bool> = OnceLock::new();
    let plain = env::var_os("NO_COLOR").is_some()
        || env::var("TERM").is_ok_and(|t| t == "dumb");
    if err {
        *ERR.get_or_init(|| io::stderr().is_terminal() && !plain)
    } else {
        *OUT.get_or_init(|| io::stdout().is_terminal() && !plain)
    }
}

pub fn paint(code: &str, s: &str, err: bool) -> String {
    if tty(err) {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String { paint("1", s, false) }
pub fn dim(s: &str) -> String { paint("2", s, false) }
pub fn green(s: &str) -> String { paint("32", s, false) }
pub fn red(s: &str) -> String { paint("31", s, false) }
pub fn yellow(s: &str) -> String { paint("33", s, false) }
pub fn cyan(s: &str) -> String { paint("36", s, false) }

pub fn ok_mark() -> String { green("✓") }
pub fn bad_mark() -> String { red("✗") }
pub fn warn_mark() -> String { yellow("!") }

/// The jolta banner, cyan→violet gradient on color terminals, plain otherwise.
pub fn banner() {
    const ART: [&str; 5] = [
        r"   __     ______     __         ______   ______",
        r"  /\ \   /\  __ \   /\ \       /\__  _\ /\  __ \",
        r" _\_\ \  \ \ \/\ \  \ \ \____  \/_/\ \/ \ \  __ \",
        r"/\_____\  \ \_____\  \ \_____\    \ \_\  \ \_\ \_\",
        r"\/_____/   \/_____/   \/_____/     \/_/   \/_/\/_/",
    ];
    const SHADES: [u8; 5] = [81, 75, 69, 63, 57];
    for (line, shade) in ART.iter().zip(SHADES) {
        println!("{}", paint(&format!("38;5;{shade}"), line, false));
    }
    // version straight from Cargo.toml at compile time — nothing to keep in sync
    let v = concat!("v", env!("CARGO_PKG_VERSION"));
    // pad BEFORE painting: ANSI escapes would break the right-alignment
    println!("{}", dim(&format!("{v:>50}")));
    println!();
}

pub fn die(msg: &str) -> ! {
    eprintln!("{} {msg}", paint("31", "jolta:", true));
    exit(1);
}

pub fn fmt_bytes(b: u64) -> String {
    if b >= 1_000_000_000 {
        format!("{:.1} GB", b as f64 / 1e9)
    } else if b >= 1_000_000 {
        format!("{:.1} MB", b as f64 / 1e6)
    } else {
        format!("{} kB", b / 1000)
    }
}

/// Spinner on stderr for phases with no per-item progress. Starts drawing only
/// after 200ms, so cache-warm paths never flash it; inert when not a TTY.
pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

pub fn spinner(label: &str) -> Spinner {
    let stop = Arc::new(AtomicBool::new(false));
    if !tty(true) {
        return Spinner { stop, handle: None };
    }
    let flag = stop.clone();
    let label = label.to_string();
    let handle = std::thread::spawn(move || {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        std::thread::sleep(Duration::from_millis(200));
        let mut i = 0;
        let mut drew = false;
        while !flag.load(Ordering::Relaxed) {
            eprint!("\r\x1b[2K\x1b[36m{}\x1b[0m {label}…", FRAMES[i % FRAMES.len()]);
            let _ = io::stderr().flush();
            drew = true;
            i += 1;
            std::thread::sleep(Duration::from_millis(100));
        }
        if drew {
            eprint!("\r\x1b[2K");
            let _ = io::stderr().flush();
        }
    });
    Spinner { stop, handle: Some(handle) }
}

impl Spinner {
    pub fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
