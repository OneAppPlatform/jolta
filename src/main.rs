//! jolta — automatic per-project JDK switching, Volta-style.
//!
//! One binary, two personalities: invoked as `jolta` it is the CLI; invoked
//! through a symlink named after a JDK tool (`java`, `javac`, ...) it acts as
//! that tool's shim — resolve the pinned JDK, set JAVA_HOME, exec the real
//! binary. Behavior is kept in lockstep with the reference sh implementation
//! on main; test/smoke.sh is the conformance suite.

mod commands;
mod complete;
mod help;
mod platform;
mod download;
mod install;
mod jdk;
mod paths;
mod resolve;
mod ui;

use std::env;
use std::ffi::OsString;
use std::path::Path;
use std::process::{exit, Command};

use commands::*;
use help::{cmd_help, usage};
use resolve::resolve_current;
use ui::die;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shim mode: resolve the pinned JDK and exec the real `tool` from it.
/// Args stay OsString end-to-end: the shim must forward argv byte-exact,
/// even bytes that aren't valid UTF-8.
fn run_shim(tool: &str, args: Vec<OsString>) -> ! {
    let r = resolve_current(true);
    let bin = jdk::tool_bin(&r.home, tool);
    if !bin.is_file() {
        die(&format!(
            "'{tool}' is not provided by the resolved JDK ({})\n  version was selected by: {}",
            r.home.display(),
            r.source
        ));
    }
    // Recursion guard (volta #630): a mis-registered JDK whose bin/java is a
    // symlink back to jolta would exec-loop forever.
    if let (Ok(me), Ok(target)) = (env::current_exe().and_then(|p| p.canonicalize()), bin.canonicalize()) {
        if target == me {
            die(&format!(
                "'{tool}' in {} resolves back to jolta itself — refusing to recurse\n  \
                 (a shim or the jolta binary is inside a registered JDK's bin; run 'jolta reshim')",
                r.home.display()
            ));
        }
    }
    let mut cmd = Command::new(&bin);
    cmd.args(&args).env("JAVA_HOME", &r.home);
    platform::exec_replace(cmd);
}

fn main() {
    platform::init();

    // args_os, not args: env::args() PANICS on argv that isn't valid UTF-8.
    // Shims forward the raw bytes; the CLI degrades lossily (an invalid spec
    // just fails to match, with a normal error).
    let mut args_os: Vec<OsString> = env::args_os().collect();
    // file_stem so "java.exe" dispatches as "java" on Windows
    let invoked = Path::new(&args_os[0])
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("jolta")
        .to_string();

    // Shim mode only when the invoked name is actually one of our shims (or
    // argv[0] lives in the shims dir). A renamed/copied binary — jolta-nightly,
    // AppImage-style ARGV0 launchers — must behave as the CLI, not die trying
    // to resolve a JDK tool named after itself (mise PR #11277 class).
    if invoked != "jolta" {
        let shims = paths::shims_dir();
        let is_shim = shims.join(format!("{invoked}{}", env::consts::EXE_SUFFIX)).exists()
            || Path::new(&args_os[0]).parent().is_some_and(|p| p == shims);
        if is_shim {
            run_shim(&invoked, args_os.split_off(1));
        }
    }
    let args: Vec<String> = args_os.iter().map(|a| a.to_string_lossy().into_owned()).collect();

    let cmd = args.get(1).cloned().unwrap_or_else(|| "help".into());
    let mut rest: Vec<String> = args.iter().skip(2).cloned().collect();
    if env::var("JOLTA_FRESH").is_ok() {
        install::set_fresh();
    }
    // --fresh belongs to the remote-cache commands only; stripping it from
    // exec's passthrough args would eat an argument meant for the child tool
    // (volta #863 class).
    if matches!(cmd.as_str(), "catalog" | "search" | "available" | "ls-remote" | "update" | "outdated" | "upgrade" | "install") {
        if let Some(i) = rest.iter().position(|a| a == "--fresh") {
            rest.remove(i);
            install::set_fresh();
        }
    }
    match cmd.as_str() {
        "setup" => cmd_setup(),
        "pin" => cmd_pin(&rest),
        "default" => match rest.first() {
            Some(s) => cmd_default(s),
            None => die("usage: jolta default <spec>"),
        },
        "install" => match rest.first() {
            Some(s) => cmd_install(s),
            None => die("usage: jolta install <spec>  (e.g. 21, corretto@21, graalvm@25)"),
        },
        "uninstall" => match rest.first() {
            Some(s) => cmd_uninstall(s),
            None => die("usage: jolta uninstall <spec>  (e.g. 25, temurin@25, or a full name like temurin-25.0.3)"),
        },
        "catalog" | "search" | "available" | "ls-remote" => cmd_catalog(rest.first().map(String::as_str)),
        "update" | "outdated" => cmd_update(),
        "upgrade" => cmd_upgrade(rest.first().map(String::as_str)),
        "list" | "ls" => cmd_list(&rest),
        "jdks" => cmd_jdks(),
        "current" => cmd_current(&rest),
        "which" => cmd_which(rest.first().map(String::as_str).unwrap_or("java")),
        "exec" => {
            if rest.is_empty() {
                die("usage: jolta exec <command> [args...]");
            }
            cmd_exec(&rest);
        }
        "env" => cmd_env(),
        "home" => cmd_home(),
        "hook" => {
            let shell = rest.first().cloned().unwrap_or_else(shell_name);
            cmd_hook(&shell);
        }
        "mirror" => install::cmd_mirror(&rest),
        "toolchains" => cmd_toolchains(&rest),
        "completions" => {
            let shell = rest.first().cloned().unwrap_or_else(shell_name);
            complete::cmd_completions(&shell);
        }
        "prune" => cmd_prune(&rest),
        "vendor" => cmd_vendor(&rest),
        "reshim" => cmd_reshim(),
        "doctor" => exit(cmd_doctor(rest.iter().any(|a| a == "--fix"))),
        "implode" => cmd_implode(&rest),
        "version" | "-v" | "--version" => println!("jolta {VERSION}"),
        "help" | "-h" | "--help" => match rest.first() {
            Some(topic) => cmd_help(topic),
            None => usage(),
        },
        other => {
            usage();
            die(&format!("unknown command '{other}'"));
        }
    }
}
