//! CLI command handlers.

use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::install::{
    install_vendor_major, install_vendor_spec, latest_remote_version, probe_latest,
    prune_superseded, release_universe, vendor_versions,
};
use crate::jdk::{
    jdk_version, list_all, list_managed, list_system, major_of, numkey, parse_spec,
    system_default, tool_bin, vendor_of, INSTALLABLE_VENDORS, KNOWN_VENDORS,
};
use crate::platform;
use crate::paths::{home_dir, jolta_home, shims_dir, which};
use crate::resolve::{clear_cache, read_pin, resolve, resolve_current};
use crate::ui::{bad_mark, bold, cyan, die, dim, green, ok_mark, paint, warn_mark, yellow};

pub fn usage() {
    print!(
        "{} — automatic per-project JDK switching (like Volta, for Java)

{}: jolta <command> [args]

  setup                  Install shims and add jolta to your shell profile
  pin <spec>             Pin a Java version for this project (.java-version)
  default <spec>         Set the global fallback Java version
  install <spec>         Download a JDK (e.g. 21, corretto@21, graalvm@25)
  update, outdated       Check jolta-managed JDKs for newer point releases
  upgrade [spec]         Upgrade jolta-managed JDKs (all, or one: 21, corretto@21)
  uninstall <name>       Remove a jolta-managed JDK (see 'jolta list' for names)
  list, ls               List installed JDKs and where they come from
  catalog [x]            The JDK catalog: latest per distro, per-major, or per-distro
                         (@v filters: temurin@21 all 21.x, temurin@21.0 all 21.0.x)
                         aliases: search, available, ls-remote
                         results cached 24h; --fresh refetches
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
    // The shims dir is wholly jolta-owned: clear every entry (Windows shims
    // can be hard links or copies, not just symlinks)
    if let Ok(entries) = fs::read_dir(&shims) {
        for entry in entries.flatten() {
            let _ = fs::remove_file(entry.path());
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
            if name.to_string_lossy().starts_with("jolta") || !platform::is_shimmable(&entry.path()) {
                continue;
            }
            let link = shims.join(&name);
            if !link.exists() && platform::make_shim(&me, &link) {
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
    let installed = home.join("bin").join(format!("jolta{}", env::consts::EXE_SUFFIX));
    if me != installed {
        // Install a self-contained copy so the build/checkout can be deleted
        let _ = fs::remove_file(&installed);
        fs::copy(&me, &installed).unwrap_or_else(|e| die(&format!("cannot install to {}: {e}", installed.display())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&installed, fs::Permissions::from_mode(0o755));
        }
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

    #[cfg(windows)]
    {
        println!("{} add these to your user PATH (Settings > Environment Variables):", warn_mark());
        println!("    {}", shims_dir().display());
        println!("    {}", home.join("bin").display());
        println!("{} then add this line to your PowerShell $PROFILE for the JAVA_HOME hook:", warn_mark());
        println!("    jolta hook powershell | Out-String | Invoke-Expression");
        println!("{} setup complete {}", ok_mark(), dim("(open a new shell to activate)"));
        return;
    }
    #[allow(unreachable_code)]
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
            if install_vendor_spec(vendor, &version).is_err() {
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
    let bin = tool_bin(&r.home, tool);
    if !bin.is_file() {
        die(&format!("'{tool}' not found in {}", r.home.display()));
    }
    println!("{}", bin.display());
}

pub fn cmd_exec(argv: &[String]) -> ! {
    let r = resolve_current(true);
    let mut dirs = vec![r.home.join("bin")];
    if let Some(old) = env::var_os("PATH") {
        dirs.extend(env::split_paths(&old));
    }
    let path = env::join_paths(dirs).unwrap_or_else(|e| die(&format!("bad PATH: {e}")));
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).env("JAVA_HOME", &r.home).env("PATH", path);
    platform::exec_replace(cmd);
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
        "powershell" | "pwsh" => print!(
            "function global:__jolta_update_java_home {{\n  $jh = & jolta home 2>$null\n  if ($LASTEXITCODE -eq 0 -and $jh) {{ $env:JAVA_HOME = \"$jh\" }}\n  else {{ Remove-Item Env:JAVA_HOME -ErrorAction SilentlyContinue }}\n}}\n$global:__jolta_prev_prompt = $function:prompt\nfunction global:prompt {{ __jolta_update_java_home; & $global:__jolta_prev_prompt }}\n__jolta_update_java_home\n"
        ),
        other => die(&format!("no hook available for shell '{other}' (zsh, bash, and powershell are supported)")),
    }
}

pub fn cmd_install(spec: &str) {
    let (vendor, version) = parse_spec(spec);
    let vendor = vendor.unwrap_or("temurin");
    major_of(&version)
        .unwrap_or_else(|| die(&format!("cannot parse version '{spec}' (try e.g. 21, 21.0.2, or corretto@21)")));
    if install_vendor_spec(vendor, &version).is_err() {
        std::process::exit(1);
    }
}

/// Set of (distro, full version) currently installed on this machine, for
/// marking listings.
fn installed_set() -> Vec<(String, String)> {
    list_all()
        .into_iter()
        .filter_map(|(v, h)| vendor_of(&h).map(|ven| (ven.to_string(), v)))
        .collect()
}

fn installed_mark(installed: &[(String, String)], vendor: &str, version: &str) -> String {
    if installed.iter().any(|(ven, v)| ven == vendor && v == version) {
        format!(" {}", green("✓ installed"))
    } else {
        String::new()
    }
}

/// jolta catalog                  -> latest per distro
/// jolta catalog 21               -> each distro's latest 21.x
/// jolta catalog temurin          -> a distro's latest per major (LTS + current)
/// jolta catalog temurin@21[.0]   -> published versions matching the filter
pub fn cmd_catalog(arg: Option<&str>) {
    let installed = installed_set();

    // Prefetch the whole dataset behind one spinner instead of stalling
    // row-by-row; the render below then reads warm cache exclusively.
    let sp = crate::ui::spinner("updating catalog");
    let universe = release_universe();
    if let Some((_, lts, _, feature)) = &universe {
        let mut majors: Vec<u32> = lts.clone();
        majors.push(*feature);
        majors.sort_by_key(|x| std::cmp::Reverse(*x));
        majors.dedup();
        let wanted: Vec<u32> = match arg {
            Some(a) if !KNOWN_VENDORS.contains(&parse_spec(a).0.unwrap_or("")) && !INSTALLABLE_VENDORS.contains(&a) => {
                major_of(a).map(|m| vec![m]).unwrap_or(majors)
            }
            _ => majors,
        };
        // fan out: every (distro, major) lookup runs its curl concurrently,
        // so a cold prefetch costs one round-trip, not thirty
        std::thread::scope(|scope| {
            for vendor in INSTALLABLE_VENDORS {
                for m in &wanted {
                    let m = *m;
                    scope.spawn(move || {
                        if latest_remote_version(vendor, m).is_none() && matches!(vendor, "oracle" | "graalvm") {
                            let _ = probe_latest(vendor, m);
                        }
                    });
                }
            }
            // filter mode additionally needs the full listing for that distro+major
            if let Some(a) = arg {
                let (v, ver) = parse_spec(a);
                if let (Some(v), false) = (v, ver.is_empty()) {
                    if let Some(m) = major_of(&ver) {
                        scope.spawn(move || {
                            let _ = vendor_versions(v, m);
                        });
                    }
                }
            }
        });
    }
    sp.finish();

    // header line: the Java release universe with LTS + newest highlighted
    if let Some((available, lts, recent_lts, recent_feature)) = &universe {
        let majors: Vec<String> = available
            .iter()
            .map(|m| {
                let s = m.to_string();
                if m == recent_feature {
                    bold(&cyan(&s))
                } else if lts.contains(m) {
                    bold(&s)
                } else {
                    dim(&s)
                }
            })
            .collect();
        println!(
            "{} {}   {}",
            bold("Java majors:"),
            majors.join(" "),
            dim(&format!("(bold = LTS, latest LTS {recent_lts}, newest {recent_feature})"))
        );
        println!();
    }

    match arg {
        // jolta available temurin  /  temurin@21
        Some(a) if KNOWN_VENDORS.contains(&parse_spec(a).0.unwrap_or("")) || INSTALLABLE_VENDORS.contains(&a) => {
            let (vendor, ver) = if INSTALLABLE_VENDORS.contains(&a) {
                (a, String::new())
            } else {
                let (v, ver) = parse_spec(a);
                (v.unwrap(), ver)
            };
            if ver.is_empty() {
                // distro overview: latest per major, LTS set + current feature
                let mut majors: Vec<u32> = universe
                    .as_ref()
                    .map(|(_, lts, _, feature)| {
                        let mut m = lts.clone();
                        m.push(*feature);
                        m
                    })
                    .unwrap_or_default();
                majors.sort_by_key(|x| std::cmp::Reverse(*x));
                majors.dedup();
                println!(
                    "{} {}",
                    bold(&cyan(vendor)),
                    dim("— latest per major (LTS + current; use @<major> for every build)")
                );
                let mut any = false;
                for major in majors {
                    match latest_remote_version(vendor, major) {
                        Some(v) => {
                            any = true;
                            let extra = if installed.iter().any(|(ven, iv)| ven == vendor && iv == &v) {
                                format!(" {}", green("✓ installed"))
                            } else {
                                let others: Vec<&str> = installed
                                    .iter()
                                    .filter(|(ven, iv)| ven == vendor && major_of(iv) == Some(major))
                                    .map(|(_, iv)| iv.as_str())
                                    .collect();
                                if others.is_empty() {
                                    String::new()
                                } else {
                                    dim(&format!(" (installed: {})", others.join(", ")))
                                }
                            };
                            println!("  {:<4} {}{extra}", bold(&major.to_string()), v);
                        }
                        None if probe_latest(vendor, major) => {
                            any = true;
                            println!("  {:<4} {}", bold(&major.to_string()), dim("published (version shown on install)"));
                        }
                        None => {}
                    }
                }
                if !any {
                    println!("  {}", dim("(nothing reachable — network problem, or distro doesn't publish these majors)"));
                }
            } else {
                // @v is a prefix filter: temurin@21 -> all 21.x, @21.0 -> 21.0.x
                let major = major_of(&ver).unwrap_or_else(|| die(&format!("cannot parse '{ver}'")));
                println!(
                    "{} {}",
                    bold(&cyan(vendor)),
                    dim(&format!("— published GA versions matching {ver}"))
                );
                if matches!(vendor, "oracle" | "graalvm") {
                    println!("{}", dim("  (no listing API; latest per major only — exact installs of any published version work)"));
                }
                let versions: Vec<String> = vendor_versions(vendor, major)
                    .into_iter()
                    .filter(|v| v == &ver || v.starts_with(&format!("{ver}.")) || v.starts_with(&format!("{ver}u")))
                    .collect();
                if versions.is_empty() {
                    println!("  {}", dim("(no published versions match)"));
                } else {
                    for (i, v) in versions.iter().enumerate() {
                        let latest_tag = if i == 0 { dim(" (latest)") } else { String::new() };
                        println!("  {v}{latest_tag}{}", installed_mark(&installed, vendor, v));
                    }
                }
            }
        }
        // jolta available 21
        Some(a) => {
            let major = major_of(a).unwrap_or_else(|| {
                die(&format!("cannot parse '{a}' (try a major like 21, or a distro like temurin)"))
            });
            println!("{} {}", bold(&format!("Latest Java {major} by distro")), dim("(✓ = installed)"));
            for vendor in INSTALLABLE_VENDORS {
                match latest_remote_version(vendor, major) {
                    Some(v) => println!("  {:<10} {}{}", cyan(vendor), bold(&v), installed_mark(&installed, vendor, &v)),
                    None if probe_latest(vendor, major) => println!(
                        "  {:<10} {}",
                        cyan(vendor),
                        dim("published (vendor hides the version until download)")
                    ),
                    None => println!("  {:<10} {}", cyan(vendor), dim("not published for this platform/major (archive may still have exact versions)")),
                }
            }
        }
        // jolta available
        None => {
            let Some((available, _, _, recent_feature)) = &universe else {
                die("cannot reach the Adoptium release index (offline?)");
            };
            let mut majors_desc = available.clone();
            majors_desc.sort_by_key(|x| std::cmp::Reverse(*x));
            println!("{} {}", bold("Latest available by distro"), dim("(✓ = installed)"));
            for vendor in INSTALLABLE_VENDORS {
                // newest major this distro actually publishes (probe downward)
                let mut hit = None;
                for major in majors_desc.iter().take(4) {
                    if let Some(v) = latest_remote_version(vendor, *major) {
                        hit = Some((*major, Some(v)));
                        break;
                    }
                    if probe_latest(vendor, *major) {
                        hit = Some((*major, None));
                        break;
                    }
                }
                match hit {
                    Some((m, v)) => {
                        let note = if m == *recent_feature { String::new() } else { dim(&format!(" (newest at {m})")) };
                        match v {
                            Some(v) => println!("  {:<10} {}{note}{}", cyan(vendor), bold(&v), installed_mark(&installed, vendor, &v)),
                            None => println!("  {:<10} {}{note}", cyan(vendor), bold(&format!("{m} (exact version shown on install)"))),
                        }
                    }
                    None => println!("  {:<10} {}", cyan(vendor), dim("unreachable")),
                }
            }
            println!("
{}", dim("jolta available <major> · jolta available <distro>[@<major>] for more"));
        }
    }
}

/// jolta-managed JDKs as (vendor, major, best installed full version),
/// one entry per distro+major (highest build wins).
fn managed_targets() -> Vec<(String, u32, String)> {
    let mut best: Vec<(String, u32, String)> = Vec::new();
    let jdks = jolta_home().join("jdks");
    let Ok(entries) = fs::read_dir(&jdks) else { return best };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some((vendor, full)) = name.split_once('-') else { continue };
        if !KNOWN_VENDORS.contains(&vendor) {
            continue;
        }
        let Some(major) = major_of(full) else { continue };
        match best.iter_mut().find(|(v, m, _)| v == vendor && *m == major) {
            Some(slot) if numkey(full) > numkey(&slot.2) => slot.2 = full.to_string(),
            Some(_) => {}
            None => best.push((vendor.to_string(), major, full.to_string())),
        }
    }
    best.sort();
    best
}

pub fn cmd_update() {
    let targets = managed_targets();
    if targets.is_empty() {
        println!(
            "no jolta-managed JDKs to check {}",
            dim("(system JDKs update through their own package managers, e.g. brew)")
        );
        return;
    }
    let mut outdated = 0;
    for (vendor, major, installed) in &targets {
        match latest_remote_version(vendor, *major) {
            Some(latest) if numkey(&latest) > numkey(installed) => {
                println!(
                    "  {} {}@{}  {} -> {}",
                    yellow("!"),
                    cyan(vendor),
                    major,
                    installed,
                    bold(&latest)
                );
                outdated += 1;
            }
            Some(_) => {
                println!("  {} {}@{}  {} {}", ok_mark(), cyan(vendor), major, installed, dim("(up to date)"));
            }
            None => {
                println!(
                    "  ? {}@{}  {} {}",
                    cyan(vendor),
                    major,
                    installed,
                    dim("(latest unknown — mirror or non-redirecting vendor; 'jolta upgrade' will check by fetching)")
                );
            }
        }
    }
    if outdated > 0 {
        println!("\n{outdated} outdated — run {} to update", bold("jolta upgrade"));
    }
}

pub fn cmd_upgrade(spec: Option<&str>) {
    crate::install::set_fresh(); // never skip a new point release on stale cache

    let managed = managed_targets();
    let targets: Vec<(String, u32, String)> = match spec {
        Some(s) => {
            let (vendor, version) = parse_spec(s);
            let vendor = vendor.unwrap_or("temurin");
            let major = major_of(&version)
                .unwrap_or_else(|| die(&format!("cannot parse '{s}' (try e.g. 21 or corretto@21)")));
            match managed.into_iter().find(|(v, m, _)| v == vendor && *m == major) {
                Some(t) => vec![t],
                None => die(&format!(
                    "{vendor}@{major} is not jolta-managed — 'jolta install {s}' to add it \
                     (system JDKs upgrade through their own package managers)"
                )),
            }
        }
        None => managed,
    };
    if targets.is_empty() {
        println!(
            "nothing to upgrade {}",
            dim("(no jolta-managed JDKs; system JDKs upgrade through their own package managers)")
        );
        return;
    }
    let mut upgraded = 0;
    for (vendor, major, installed) in &targets {
        if let Some(latest) = latest_remote_version(vendor, *major) {
            if numkey(&latest) <= numkey(installed) {
                println!("  {} {}@{}  {} {}", ok_mark(), cyan(vendor), major, installed, dim("(up to date)"));
                continue;
            }
        }
        // unknown or newer: fetch latest; install reports "already installed"
        // if it lands on the same version we have
        match install_vendor_major(vendor, *major) {
            Ok(landed) => {
                if numkey(&landed) > numkey(installed) {
                    prune_superseded(vendor, *major, &landed);
                    upgraded += 1;
                }
            }
            Err(()) => eprintln!(
                "{} upgrade of {vendor}@{major} failed; kept {installed}",
                paint("33", "jolta: warning:", true)
            ),
        }
    }
    if upgraded > 0 {
        println!("{} upgraded {upgraded} JDK(s)", ok_mark());
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
            "This removes {} and the jolta lines from {}.",
            home.display(),
            profile.display()
        );
        let managed = list_managed();
        if managed.is_empty() {
            println!("No jolta-installed JDKs to delete.");
        } else {
            println!("The following jolta-installed JDKs will be deleted:");
            for (v, h) in &managed {
                println!("  - {} ({})", v, h.display());
            }
        }
        println!("JDKs installed outside jolta (Homebrew, /Library/Java, SDKMAN, ...) are not touched.");
        print!("Type \"yes\" to continue: ");
        let _ = io::stdout().flush();
        let mut answer = String::new();
        let _ = io::stdin().lock().read_line(&mut answer);
        if answer.trim() != "yes" {
            die("aborted");
        }
    }
    #[cfg(windows)]
    let _ = &profile;
    #[cfg(unix)]
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

    let on_path = env::var_os("PATH")
        .map(|p| env::split_paths(&p).any(|d| d == shims_dir()))
        .unwrap_or(false);
    if on_path {
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
