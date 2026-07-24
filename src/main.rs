//! jolta — automatic per-project JDK switching, Volta-style.
//!
//! One binary, two personalities: invoked as `jolta` it is the CLI; invoked
//! through a symlink named after a JDK tool (`java`, `javac`, ...) it acts as
//! that tool's shim — resolve the pinned JDK, set JAVA_HOME, exec the real
//! binary. Behavior is kept in lockstep with the reference sh implementation
//! on main; test/smoke.sh is the conformance suite.

mod commands;
mod download;
mod install;
mod jdk;
mod paths;
mod resolve;
mod ui;

use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{exit, Command};

use commands::*;
use resolve::resolve_current;
use ui::die;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shim mode: resolve the pinned JDK and exec the real `tool` from it.
fn run_shim(tool: &str, args: Vec<String>) -> ! {
    let r = resolve_current(true);
    let bin = r.home.join("bin").join(tool);
    if !bin.is_file() {
        die(&format!(
            "'{tool}' is not provided by the resolved JDK ({})\n  version was selected by: {}",
            r.home.display(),
            r.source
        ));
    }
    let err = Command::new(&bin)
        .args(&args)
        .env("JAVA_HOME", &r.home)
        .exec();
    die(&format!("failed to exec {}: {err}", bin.display()));
}

fn main() {
    let mut args: Vec<String> = env::args().collect();
    let invoked = Path::new(&args[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("jolta")
        .to_string();

    if invoked != "jolta" {
        run_shim(&invoked, args.split_off(1));
    }

    let cmd = args.get(1).cloned().unwrap_or_else(|| "help".into());
    let rest: Vec<String> = args.iter().skip(2).cloned().collect();
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
            None => die(&format!(
                "usage: jolta uninstall <name>  (a directory name from {}/jdks)",
                paths::jolta_home().display()
            )),
        },
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
