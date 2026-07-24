//! CLI command handlers.

use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::install::install_vendor_major;
use crate::jdk::{
    jdk_version, list_all, list_managed, list_system, major_of, parse_spec, system_default,
    vendor_of, INSTALLABLE_VENDORS,
};
use crate::paths::{home_dir, jolta_home, shims_dir, which};
use crate::resolve::{clear_cache, read_pin, resolve, resolve_current};
use crate::ui::{bad_mark, bold, cyan, die, dim, green, ok_mark, paint, warn_mark};

pub fn usage() {
    print!(
        "{} — automatic per-project JDK switching (like Volta, for Java)

{}: jolta <command> [args]

  setup                  Install shims and add jolta to your shell profile
  pin <spec>             Pin a Java version for this project (.java-version)
  default <spec>         Set the global fallback Java version
  install <spec>         Download a JDK (e.g. 21, corretto@21, graalvm@25)
  uninstall <name>       Remove a jolta-managed JDK (see 'jolta list' for names)
  list, ls               List installed JDKs and where they come from
  jdks                   Machine-readable list: major<TAB>version<TAB>distro<TAB>home
  current                Show the Java version resolved for this directory
  which [tool]           Show the full path the shim would exec (default: java)
  exec <cmd> [args]      Run a command with JAVA_HOME and PATH set for this project
  env                    Print export statements for eval in scripts
  home                   Print the resolved JAVA_HOME for this directory
  hook [zsh|bash]        Print shell hook code that keeps JAVA_HOME in sync on cd
  reshim                 Regenerate shims from installed JDKs
  doctor                 Diagnose common setup problems
  implode                Uninstall jolta completely (~/.jolta + shell profile lines)
  version                Print jolta's version

A <spec> is a version with an optional distro {}:
  21   21.0.4   1.8   corretto@21   graalvm-25   temurin@8
Downloadable distros: {} (default temurin).
Distro-less pins match any installed JDK of that major version.

Version resolution order:
  JOLTA_JAVA_VERSION env var  >  nearest .java-version (walking up)  >
  jolta default  >  system default JDK
",
        bold("jolta"),
        bold("Usage"),
        dim("(distro@version or distro-version)"),
        cyan(&INSTALLABLE_VENDORS.join(", "))
    );
}

pub fn cmd_reshim() {
    let shims = shims_dir();
    let _ = fs::create_dir_all(&shims);
    if let Ok(entries) = fs::read_dir(&shims) {
        for entry in entries.flatten() {
            if entry.path().symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    let me = env::current_exe().unwrap_or_else(|_| die("cannot locate the jolta binary"));
    let mut count = 0u32;
    let mut homes: Vec<PathBuf> = list_all().into_iter().map(|(_, h)| h).collect();
    homes.sort();
    homes.dedup();
    for home in homes {
        let Ok(entries) = fs::read_dir(home.join("bin")) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == "jolta" {
                continue;
            }
            let link = shims.join(&name);
            if !link.exists() && symlink(&me, &link).is_ok() {
                count += 1;
            }
        }
    }
    clear_cache();
    println!("{} {count} shims in {}", ok_mark(), shims.display());
}

fn profile_file() -> PathBuf {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let shell = Path::new(&shell).file_name().and_then(|s| s.to_str()).unwrap_or("zsh").to_string();
    match shell.as_str() {
        "zsh" => home_dir().join(".zshrc"),
        "bash" => {
            let bp = home_dir().join(".bash_profile");
            if bp.is_file() { bp } else { home_dir().join(".bashrc") }
        }
        _ => home_dir().join(".profile"),
    }
}

pub fn shell_name() -> String {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    Path::new(&shell).file_name().and_then(|s| s.to_str()).unwrap_or("zsh").to_string()
}

pub fn cmd_setup() {
    let home = jolta_home();
    for sub in ["bin", "jdks", "cache", "shims"] {
        let _ = fs::create_dir_all(home.join(sub));
    }
    let me = env::current_exe().unwrap_or_else(|_| die("cannot locate the jolta binary"));
    let installed = home.join("bin/jolta");
    if me != installed {
        // Install a self-contained copy so the build/checkout can be deleted
        let _ = fs::remove_file(&installed);
        fs::copy(&me, &installed).unwrap_or_else(|e| die(&format!("cannot install to {}: {e}", installed.display())));
        let _ = fs::set_permissions(&installed, fs::Permissions::from_mode(0o755));
        println!(
            "{} installed to {} {}",
            ok_mark(),
            home.display(),
            dim("(this checkout is no longer needed at runtime)")
        );
        // reshim via the installed copy so shims point at it
        let st = Command::new(&installed).arg("reshim").status();
        if !st.is_ok_and(|s| s.success()) {
            die("reshim via installed copy failed");
        }
    } else {
        cmd_reshim();
    }

    let profile = profile_file();
    let existing = fs::read_to_string(&profile).unwrap_or_default();
    let mut additions = String::new();
    if existing.contains(">>> jolta >>>") {
        println!("{} PATH setup already in {}", ok_mark(), profile.display());
    } else {
        additions.push_str(
            "\n# >>> jolta >>>\nexport JOLTA_HOME=\"$HOME/.jolta\"\nexport PATH=\"$JOLTA_HOME/shims:$JOLTA_HOME/bin:$PATH\"\n# <<< jolta <<<\n",
        );
        println!("{} added PATH setup to {}", ok_mark(), profile.display());
    }
    if existing.contains("jolta hook") {
        println!("{} JAVA_HOME hook already in {}", ok_mark(), profile.display());
    } else {
        additions.push_str(&format!(
            "\n# >>> jolta hook (keeps JAVA_HOME in sync with your cwd) >>>\neval \"$(jolta hook {})\"\n# <<< jolta hook <<<\n",
            shell_name()
        ));
        println!("{} added JAVA_HOME hook to {}", ok_mark(), profile.display());
    }
    if !additions.is_empty() {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&profile)
            .unwrap_or_else(|e| die(&format!("cannot open {}: {e}", profile.display())));
        f.write_all(additions.as_bytes())
            .unwrap_or_else(|e| die(&format!("cannot write {}: {e}", profile.display())));
    }
    println!("{} setup complete {}", ok_mark(), dim("(open a new shell to activate)"));
}

pub fn cmd_pin(spec: &str) {
    if resolve(spec).is_none() {
        let (vendor, version) = parse_spec(spec);
        let vendor = vendor.unwrap_or("temurin");
        if env::var("JOLTA_NO_AUTO_INSTALL").is_err()
            && INSTALLABLE_VENDORS.contains(&vendor)
            && major_of(&version).is_some()
        {
            eprintln!(
                "{} no installed JDK matches '{spec}' — fetching it now",
                paint("33", "jolta:", true)
            );
            let major = major_of(&version).unwrap();
            if install_vendor_major(vendor, major).is_err() {
                eprintln!(
                    "{} install failed; pin written anyway ('jolta install {spec}' to retry)",
                    paint("33", "jolta: warning:", true)
                );
            }
        } else {
            eprintln!(
                "{} no installed JDK currently matches '{spec}' ('jolta install {spec}' to fetch it)",
                paint("33", "jolta: warning:", true)
            );
        }
    }
    fs::write(".java-version", format!("{spec}\n")).unwrap_or_else(|e| die(&format!("cannot write .java-version: {e}")));
    let cwd = env::current_dir().unwrap_or_default();
    println!("{} pinned Java {} in {}/.java-version", ok_mark(), bold(spec), cwd.display());
}

pub fn cmd_default(spec: &str) {
    if resolve(spec).is_none() {
        eprintln!(
            "{} no installed JDK currently matches '{spec}'",
            paint("33", "jolta: warning:", true)
        );
    }
    let _ = fs::create_dir_all(jolta_home());
    fs::write(jolta_home().join("default"), format!("{spec}\n")).unwrap_or_else(|e| die(&format!("cannot write default: {e}")));
    println!("{} default Java version set to {}", ok_mark(), bold(spec));
}

pub fn cmd_list() {
    let pin = read_pin();
    let current = match &pin.spec {
        Some(spec) => resolve(spec),
        None => system_default(),
    };
    let row = |v: &str, h: &PathBuf| {
        let active = Some(h) == current.as_ref();
        let vendor = vendor_of(h).unwrap_or("?");
        if active {
            format!(
                "  {} {}",
                green("*"),
                bold(&format!("{:<12} {:<9} {}", v, vendor, h.display()))
            )
        } else {
            format!(
                "    {v:<12} {} {}",
                cyan(&format!("{vendor:<9}")),
                dim(&h.display().to_string())
            )
        }
    };
    println!(
        "{} {}:",
        bold("jolta-managed"),
        dim(&format!("({}/jdks)", jolta_home().display()))
    );
    let managed = list_managed();
    if managed.is_empty() {
        println!("    {}", dim("(none — use \"jolta install <spec>\")"));
    } else {
        for (v, h) in &managed {
            println!("{}", row(v, h));
        }
    }
    println!("{}:", bold("system"));
    let mut system = list_system();
    system.sort();
    system.dedup();
    for (v, h) in &system {
        println!("{}", row(v, h));
    }
    match &pin.spec {
        Some(spec) => println!(
            "\n{} = active here {}",
            green("*"),
            dim(&format!("(pinned '{spec}' by {})", pin.source))
        ),
        None => println!(
            "\n{} = active here {}",
            green("*"),
            dim("(system default; nothing pinned)")
        ),
    }
}

pub fn cmd_jdks() {
    let mut all = list_all();
    all.sort();
    all.dedup();
    for (v, h) in all {
        if let Some(m) = major_of(&v) {
            println!("{m}\t{v}\t{}\t{}", vendor_of(&h).unwrap_or("?"), h.display());
        }
    }
}

pub fn cmd_current() {
    let r = resolve_current(false);
    let v = jdk_version(&r.home).unwrap_or_else(|| "unknown".into());
    let vendor = vendor_of(&r.home).map(|s| format!(" ({s})")).unwrap_or_default();
    println!("{}{} {}", bold(&v), cyan(&vendor), dim(&format!("(from {})", r.source)));
    println!("{}", r.home.display());
}

pub fn cmd_which(tool: &str) {
    let r = resolve_current(false);
    let bin = r.home.join("bin").join(tool);
    if !bin.is_file() {
        die(&format!("'{tool}' not found in {}", r.home.display()));
    }
    println!("{}", bin.display());
}

pub fn cmd_exec(argv: &[String]) -> ! {
    let r = resolve_current(true);
    let path = format!(
        "{}/bin:{}",
        r.home.display(),
        env::var("PATH").unwrap_or_default()
    );
    let err = Command::new(&argv[0])
        .args(&argv[1..])
        .env("JAVA_HOME", &r.home)
        .env("PATH", path)
        .exec();
    die(&format!("failed to exec {}: {err}", argv[0]));
}

pub fn cmd_env() {
    let r = resolve_current(true);
    println!("export JAVA_HOME=\"{}\"", r.home.display());
    println!("export PATH=\"{}/bin:$PATH\"", r.home.display());
}

pub fn cmd_home() {
    let r = resolve_current(false);
    println!("{}", r.home.display());
}

pub fn cmd_hook(shell: &str) {
    match shell {
        "zsh" => print!(
            "_jolta_update_java_home() {{\n  local _jh\n  if _jh=$(jolta home 2>/dev/null); then\n    export JAVA_HOME=$_jh\n  else\n    unset JAVA_HOME\n  fi\n}}\nautoload -Uz add-zsh-hook\nadd-zsh-hook chpwd _jolta_update_java_home\n_jolta_update_java_home\n"
        ),
        "bash" => print!(
            "_jolta_update_java_home() {{\n  if [ \"${{_JOLTA_LAST_PWD:-}}\" = \"$PWD\" ]; then return; fi\n  _JOLTA_LAST_PWD=$PWD\n  local _jh\n  if _jh=$(jolta home 2>/dev/null); then\n    export JAVA_HOME=$_jh\n  else\n    unset JAVA_HOME\n  fi\n}}\ncase \";$PROMPT_COMMAND;\" in\n  *\";_jolta_update_java_home;\"*) ;;\n  *) PROMPT_COMMAND=\"_jolta_update_java_home${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}\" ;;\nesac\n_jolta_update_java_home\n"
        ),
        other => die(&format!("no hook available for shell '{other}' (zsh and bash are supported)")),
    }
}

pub fn cmd_install(spec: &str) {
    let (vendor, version) = parse_spec(spec);
    let vendor = vendor.unwrap_or("temurin");
    let major = major_of(&version)
        .unwrap_or_else(|| die(&format!("cannot parse version '{spec}' (try e.g. 21 or corretto@21)")));
    if install_vendor_major(vendor, major).is_err() {
        std::process::exit(1);
    }
}

pub fn cmd_uninstall(name: &str) {
    let dest = jolta_home().join("jdks").join(name);
    if !dest.is_dir() {
        die(&format!("'{name}' is not a jolta-managed JDK (see 'jolta list')"));
    }
    fs::remove_dir_all(&dest).unwrap_or_else(|e| die(&format!("cannot remove {}: {e}", dest.display())));
    clear_cache();
    println!("{} removed {}", ok_mark(), dest.display());
}

pub fn cmd_implode(args: &[String]) {
    let home = jolta_home();
    let profile = profile_file();
    if args.first().map(String::as_str) != Some("--yes") {
        println!(
            "This removes {} (including downloaded JDKs) and the jolta lines from {}.",
            home.display(),
            profile.display()
        );
        print!("Type \"yes\" to continue: ");
        let _ = io::stdout().flush();
        let mut answer = String::new();
        let _ = io::stdin().lock().read_line(&mut answer);
        if answer.trim() != "yes" {
            die("aborted");
        }
    }
    if let Ok(text) = fs::read_to_string(&profile) {
        let mut out = String::new();
        let mut skipping = false;
        for line in text.lines() {
            if line.contains("# >>> jolta hook") || line.contains("# >>> jolta >>>") {
                skipping = true;
            }
            if !skipping {
                out.push_str(line);
                out.push('\n');
            }
            if line.contains("# <<< jolta hook <<<") || line.contains("# <<< jolta <<<") {
                skipping = false;
            }
        }
        if fs::write(&profile, out).is_ok() {
            println!("{} removed jolta lines from {}", ok_mark(), profile.display());
        }
    }
    let _ = fs::remove_dir_all(&home);
    println!("{} removed {} — open a new shell to finish. So long!", ok_mark(), home.display());
}

pub fn cmd_doctor() -> i32 {
    let mut rc = 0;
    let home = jolta_home();
    println!("{}", bold("jolta doctor"));
    println!("  jolta home:    {}", home.display());
    println!(
        "  binary:        {}",
        env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "?".into())
    );

    let shim_count = fs::read_dir(shims_dir())
        .map(|e| e.flatten().filter(|x| x.path().symlink_metadata().is_ok_and(|m| m.file_type().is_symlink())).count())
        .unwrap_or(0);
    if shim_count > 0 {
        println!("  shims:         {} ok ({shim_count} installed)", ok_mark());
    } else {
        println!("  shims:         {} MISSING — run \"jolta setup\"", bad_mark());
        rc = 1;
    }

    let path = env::var("PATH").unwrap_or_default();
    let shims = shims_dir().display().to_string();
    if path.split(':').any(|d| d == shims) {
        println!("  PATH:          {} ok (shims dir is on PATH)", ok_mark());
    } else {
        println!("  PATH:          {} shims dir NOT on PATH — run \"jolta setup\" and open a new shell", bad_mark());
        rc = 1;
    }

    match which("java") {
        Some(p) if p.starts_with(shims_dir()) => {
            println!("  java:          {} ok (resolves to the jolta shim)", ok_mark())
        }
        Some(p) => {
            println!("  java:          {} BYPASSING jolta ({} comes before the shims on PATH)", bad_mark(), p.display());
            rc = 1;
        }
        None => {
            println!("  java:          {} not found on PATH", bad_mark());
            rc = 1;
        }
    }

    let expected = {
        let pin = read_pin();
        match pin.spec {
            Some(s) => resolve(&s),
            None => system_default(),
        }
    };
    match env::var("JAVA_HOME") {
        Ok(jh) if Some(PathBuf::from(&jh)) == expected => {
            println!("  JAVA_HOME:     {} ok ({jh} — matches the pin here, hook is working)", ok_mark());
        }
        Ok(jh) => {
            println!("  JAVA_HOME:     {} STALE: {jh}", bad_mark());
            println!(
                "                 expected {} for this directory",
                expected.map(|p| p.display().to_string()).unwrap_or_else(|| "<unresolvable>".into())
            );
            println!("                 mvn/gradle use JAVA_HOME directly and will bypass jolta;");
            println!("                 remove any manual \"export JAVA_HOME\" from your shell profile");
            println!("                 and make sure the jolta hook line comes after it (\"jolta setup\" adds it)");
            rc = 1;
        }
        Err(_) => {
            println!("  JAVA_HOME:     {} not set — shims still work, but mvn/gradle prefer JAVA_HOME;", warn_mark());
            println!("                 run \"jolta setup\" to install the cd hook that keeps it in sync");
        }
    }

    let count = list_all().len();
    println!("  JDKs found:    {count}");
    if count == 0 {
        println!("                 {} none! install one with \"jolta install 21\"", bad_mark());
        rc = 1;
    }

    let pin = read_pin();
    match pin.spec {
        Some(s) => println!("  pin here:      '{}' via {}", bold(&s), pin.source),
        None => println!("  pin here:      none (system default JDK will be used)"),
    }
    rc
}
