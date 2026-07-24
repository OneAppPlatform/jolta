//! OS-specific glue: signal setup, process replacement, shim link creation.

use std::path::Path;
use std::process::Command;

use crate::ui::die;

/// Rust ignores SIGPIPE by default, turning `jolta list | head` into a
/// "failed printing to stdout" panic. Restore normal Unix behavior. No-op on
/// Windows, which has no SIGPIPE.
#[cfg(unix)]
pub fn init() {
    unsafe {
        extern "C" {
            fn signal(signum: i32, handler: usize) -> usize;
        }
        const SIGPIPE: i32 = 13;
        const SIG_DFL: usize = 0;
        signal(SIGPIPE, SIG_DFL);
    }
}

#[cfg(windows)]
pub fn init() {}

/// Replace this process with `cmd` (Unix), or run it and exit with its code
/// (Windows, which has no exec).
#[cfg(unix)]
pub fn exec_replace(mut cmd: Command) -> ! {
    use std::os::unix::process::CommandExt;
    let err = cmd.exec();
    die(&format!("failed to exec: {err}"));
}

#[cfg(windows)]
pub fn exec_replace(mut cmd: Command) -> ! {
    match cmd.status() {
        Ok(st) => std::process::exit(st.code().unwrap_or(1)),
        Err(e) => die(&format!("failed to run command: {e}")),
    }
}

/// Create a shim entry at `link` invoking the jolta binary `me`.
#[cfg(unix)]
pub fn make_shim(me: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(me, link).is_ok()
}

/// Plain symlinks require Developer Mode on Windows; hard links work
/// unprivileged on the same volume, and a copy always works.
#[cfg(windows)]
pub fn make_shim(me: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_file(me, link).is_ok()
        || std::fs::hard_link(me, link).is_ok()
        || std::fs::copy(me, link).is_ok()
}

/// Should this JDK bin/ entry get a shim? Unix: any executable; Windows: .exe.
#[cfg(unix)]
pub fn is_shimmable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
pub fn is_shimmable(path: &Path) -> bool {
    path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}
