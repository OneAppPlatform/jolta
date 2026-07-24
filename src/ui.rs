//! Terminal styling: colors, glyphs, byte formatting, fatal errors.
//! Everything degrades to plain text when the stream is not a terminal,
//! and respects NO_COLOR / TERM=dumb.

use std::env;
use std::io::{self, IsTerminal};
use std::process::exit;
use std::sync::OnceLock;

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
