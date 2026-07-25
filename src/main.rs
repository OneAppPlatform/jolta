//! jolta — automatic per-project JDK switching, Volta-style.
//!
//! One binary, two personalities: invoked as `jolta` it is the CLI; invoked
//! through a symlink named after a JDK tool (`java`, `javac`, ...) it acts as
//! that tool's shim — resolve the pinned JDK, set JAVA_HOME, exec the real
//! binary. Behavior is kept in lockstep with the reference sh implementation
//! on main; test/smoke.sh is the conformance suite.

mod commands;
mod platform;
mod download;
mod install;
mod jdk;
mod paths;
mod resolve;
mod ui;

use std::env;
use std::path::Path;
use std::process::{exit, Command};

use commands::*;
use resolve::resolve_current;
use ui::die;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shim mode: resolve the pinned JDK and exec the real `tool` from it.
fn run_shim(tool: &str, args: Vec<String>) -> ! {
    let r = resolve_current(true);
    let bin = jdk::tool_bin(&r.home, tool);
    if !bin.is_file() {
        die(&format!(
            "'{tool}' is not provided by the resolved JDK ({})\n  version was selected by: {}",
            r.home.display(),
            r.source
        ));
    }
    let mut cmd = Command::new(&bin);
    cmd.args(&args).env("JAVA_HOME", &r.home);
    platform::exec_replace(cmd);
}

fn main() {
    platform::init();

    let mut args: Vec<String> = env::args().collect();
    // file_stem so "java.exe" dispatches as "java" on Windows
    let invoked = Path::new(&args[0])
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("jolta")
        .to_string();

    if invoked != "jolta" {
        run_shim(&invoked, args.split_off(1));
    }

    let cmd = args.get(1).cloned().unwrap_or_else(|| "help".into());
    let mut rest: Vec<String> = args.iter().skip(2).cloned().collect();
    if env::var("JOLTA_FRESH").is_ok() {
        install::set_fresh();
    }
    if let Some(i) = rest.iter().position(|a| a == "--fresh") {
        rest.remove(i);
        install::set_fresh();
    }
    match cmd.as_str() {
        "setup" => cmd_setup(),
        "pin" => match rest.first() {
            Some(s) => cmd_pin(s),
            None => die("usage: jolta pin <spec>  (e.g. 21 or corretto@21)"),
        },
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
        "list" | "ls" => cmd_list(),
        "jdks" => cmd_jdks(),
        "current" => cmd_current(),
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
        "reshim" => cmd_reshim(),
        "doctor" => exit(cmd_doctor()),
        "implode" => cmd_implode(&rest),
        "version" | "-v" | "--version" => println!("jolta {VERSION}"),
        "help" | "-h" | "--help" => usage(),
        other => {
            usage();
            die(&format!("unknown command '{other}'"));
        }
    }
}
