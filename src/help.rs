//! The help system: the command overview (`jolta help`) and the per-command
//! deep dives (`jolta help <command>`).

use crate::jdk::{default_vendor, INSTALLABLE_VENDORS};
use crate::ui::{bold, cyan, die, dim};

pub fn usage() {
    // pad BEFORE painting: ANSI escapes would break {:<21} alignment
    let row = |cmd: &str, desc: &str| println!("  {} {desc}", cyan(&format!("{cmd:<21}")));

    println!("{} — automatic per-project JDK switching (like Volta, for Java)\n", bold("jolta"));
    println!("{}: jolta <command> [args]\n", bold("Usage"));
    row("setup", "Install shims and add jolta to your shell profile");
    row("pin <spec>", "Pin a Java version for this project (.java-version)");
    row("default <spec>", "Set the global fallback Java version");
    row("install <spec>", "Download a JDK (e.g. 21, corretto@21, graalvm@25)");
    row("update, outdated", "Check jolta-managed JDKs for newer point releases");
    row("upgrade [spec]", "Upgrade jolta-managed JDKs (all, or one: 21, corretto@21)");
    row("uninstall <spec>", "Remove a jolta-managed JDK (25, temurin@25, or a full name)");
    row("prune [spec] [-n]", "Remove superseded builds and stale non-LTS majors");
    row("vendor [name]", "Show or set the preferred distro for vendorless specs");
    row("list, ls", "List installed JDKs and where they come from");
    row("catalog [x]", "The JDK catalog: latest per distro, per-major, or per-distro");
    row("jdks", "Machine-readable list: major<TAB>version<TAB>distro<TAB>home");
    row("current", "Show the Java version resolved for this directory");
    row("which [tool]", "Show the full path the shim would exec (default: java)");
    row("exec <cmd> [args]", "Run a command with JAVA_HOME and PATH set for this project");
    row("env", "Print export statements for eval in scripts");
    row("home", "Print the resolved JAVA_HOME for this directory");
    row("hook [shell]", "Print shell hook code that keeps JAVA_HOME in sync");
    row("completions [shell]", "Print shell completions (zsh, bash, fish)");
    row("toolchains [--write]", "Maven toolchains.xml (+ Gradle hint) from installed JDKs");
    row("mirror [sync|verify]", "Build or check an offline JDK mirror for JOLTA_DOWNLOAD_BASE");
    row("reshim", "Regenerate shims from installed JDKs");
    row("doctor", "Diagnose common setup problems");
    row("implode", "Uninstall jolta completely (~/.jolta + shell profile lines)");
    row("version", "Print jolta's version");
    println!(
        "\nA <spec> is a version with an optional distro: {}",
        cyan("21  21.0.4  corretto@21  temurin@8")
    );
    // the default vendor is the bold one
    let distros: Vec<String> = INSTALLABLE_VENDORS
        .iter()
        .map(|v| if *v == default_vendor() { bold(&cyan(v)) } else { cyan(v) })
        .collect();
    println!("Distros: {}", distros.join(", "));
    println!();
    println!("Run {} for a detailed description and examples.", cyan("jolta help <command>"));
}

/// One `jolta help <command>` page. `{default}` and `{distros}` in any text
/// field are filled in at render time so the pages never drift from the
/// actual configuration.
pub struct HelpPage {
    pub name: &'static str,
    pub summary: &'static str,
    usage: &'static [&'static str],
    aliases: &'static [&'static str],
    about: &'static [&'static str],
    examples: &'static [(&'static str, &'static str)],
}

/// Also the source of truth for completion scripts (src/complete.rs).
pub const PAGES: &[HelpPage] = &[
    HelpPage {
        name: "setup",
        summary: "install shims and wire up your shell",
        usage: &["jolta setup"],
        aliases: &[],
        about: &[
            "Creates $JOLTA_HOME (~/.jolta), installs the jolta binary into bin/, \
             and generates a shim for every JDK tool (java, javac, jshell, ...) in \
             shims/. Homebrew installs are linked through brew's stable opt path \
             instead of copied, so 'brew upgrade jolta' flows through automatically.",
            "Also adds two blocks to your shell profile: PATH setup that puts the \
             shims first, and a hook that keeps JAVA_HOME in sync with whichever \
             pin applies. On Windows the user PATH is edited in the registry and the \
             hook goes into your PowerShell profiles. Safe to re-run at any time — \
             existing profile lines are never duplicated.",
        ],
        examples: &[("jolta setup", "install, then open a new shell to activate")],
    },
    HelpPage {
        name: "pin",
        summary: "pin a Java version for this project",
        usage: &["jolta pin <spec> [--resolved]"],
        aliases: &[],
        about: &[
            "Writes <spec> to .java-version in the current directory. Every java, \
             javac, and friends run in this directory tree resolves to a matching \
             JDK — the nearest .java-version above the working directory wins, \
             then the global default.",
            "Distro-less pins (21) match any installed JDK of that major version; \
             distro pins (corretto@21) only match that distro. The keywords lts \
             and latest expand to the current LTS / feature major. If no installed \
             JDK matches, jolta downloads one on the spot (set \
             JOLTA_NO_AUTO_INSTALL=1 to disable).",
            "--resolved pins the exact version the spec lands on (21 becomes \
             21.0.4) so CI and teammates can't drift onto a different point \
             release.",
            "Commit .java-version so teammates and CI resolve the same JDK. An \
             existing SDKMAN .sdkmanrc is honored when no .java-version claims the \
             directory.",
        ],
        examples: &[
            ("jolta pin 21", "any installed 21.x"),
            ("jolta pin corretto@21", "Amazon Corretto specifically"),
            ("jolta pin lts", "the current LTS major"),
            ("jolta pin 21 --resolved", "exact: writes e.g. 21.0.4"),
        ],
    },
    HelpPage {
        name: "default",
        summary: "set the global fallback Java version",
        usage: &["jolta default <spec>"],
        aliases: &[],
        about: &[
            "Stores <spec> in $JOLTA_HOME/default. It is used whenever no \
             .java-version is found walking up from the working directory — your \
             machine-wide Java. Project pins always win over it. The keywords lts \
             and latest expand to the current LTS / feature major.",
            "Your first 'jolta install' sets the default automatically so plain \
             'java' works right away.",
        ],
        examples: &[
            ("jolta default 21", "any installed 21.x"),
            ("jolta default temurin@25", "a specific distro"),
            ("jolta default lts", "the current LTS major"),
        ],
    },
    HelpPage {
        name: "install",
        summary: "download a JDK",
        usage: &["jolta install <spec> [--fresh]"],
        aliases: &[],
        about: &[
            "Downloads and unpacks a JDK into $JOLTA_HOME/jdks, then regenerates \
             the shims. A <spec> is a major (21), an exact version (21.0.4), a \
             distro-qualified form (corretto@21, graalvm-25), or the keywords lts \
             / latest for the current LTS / feature major.",
            "Vendorless specs fetch your preferred distro (see 'jolta vendor') — \
             currently {default}. Downloadable distros: {distros}.",
            "Downloads are verified against the published SHA-256 checksum when \
             the vendor provides one. Set JOLTA_DOWNLOAD_BASE to install from an \
             offline mirror (see 'jolta help mirror'). --fresh bypasses the 24h \
             release-metadata cache.",
        ],
        examples: &[
            ("jolta install 21", "latest 21.x of the default distro"),
            ("jolta install corretto@21", "latest Corretto 21"),
            ("jolta install graalvm@25", "GraalVM 25"),
            ("jolta install 21.0.4", "an exact point release"),
        ],
    },
    HelpPage {
        name: "update",
        summary: "check managed JDKs for newer point releases",
        usage: &["jolta update [--fresh]"],
        aliases: &["outdated"],
        about: &[
            "Compares every jolta-managed JDK against the newest point release its \
             distro publishes and reports what is outdated. Read-only: nothing is \
             downloaded or changed — run 'jolta upgrade' to act on the report.",
            "System JDKs (Homebrew, /Library/Java, ...) are not checked; they \
             update through their own package managers.",
        ],
        examples: &[
            ("jolta update", "report outdated JDKs"),
            ("jolta update --fresh", "ignore cached release metadata"),
        ],
    },
    HelpPage {
        name: "upgrade",
        summary: "upgrade managed JDKs to the newest point release",
        usage: &["jolta upgrade [<spec>]"],
        aliases: &[],
        about: &[
            "Fetches the newest point release for every jolta-managed JDK, or for \
             just one distro+major (21, corretto@21). Release metadata is always \
             refetched, so a brand-new release is never missed to a stale cache.",
            "After a successful upgrade the superseded older build is removed — \
             unless a project pin references it exactly.",
        ],
        examples: &[
            ("jolta upgrade", "everything jolta manages"),
            ("jolta upgrade corretto@21", "one distro+major"),
        ],
    },
    HelpPage {
        name: "uninstall",
        summary: "remove a jolta-managed JDK",
        usage: &["jolta uninstall <spec>"],
        aliases: &[],
        about: &[
            "Removes a JDK from $JOLTA_HOME/jdks and regenerates the shims. Takes \
             the same spec forms as install (25, 21.0.4, temurin@25) or a full \
             install name as shown by 'jolta list' (temurin-25.0.3).",
            "If the spec matches more than one installed JDK, nothing is removed — \
             jolta lists ready-to-paste commands that each pick exactly one. \
             System JDKs are never touched.",
        ],
        examples: &[
            ("jolta uninstall 17", "the only installed 17"),
            ("jolta uninstall temurin@25", "by distro and major"),
            ("jolta uninstall temurin-25.0.3", "by full install name"),
        ],
    },
    HelpPage {
        name: "prune",
        summary: "remove superseded builds and stale non-LTS majors",
        usage: &["jolta prune [<spec>] [-n | --dry-run]"],
        aliases: &[],
        about: &[
            "Cleans $JOLTA_HOME/jdks in two passes: within each distro+major only \
             the newest build is kept, and whole majors are dropped when they are \
             non-LTS, not the current feature release, and not that distro's \
             newest install.",
            "Anything a project pin references survives — pins are re-read live \
             from your projects' own .java-version files, so a checkout you still \
             work in keeps its JDK.",
            "A spec (17, temurin@17) scopes the cleanup to one major or \
             distro+major. -n / --dry-run previews what would be removed without \
             deleting anything.",
        ],
        examples: &[
            ("jolta prune -n", "preview what would go"),
            ("jolta prune", "clean everything"),
            ("jolta prune temurin@17", "only temurin 17 builds"),
        ],
    },
    HelpPage {
        name: "vendor",
        summary: "show or set the preferred distro for vendorless specs",
        usage: &["jolta vendor [<name> | --unset]"],
        aliases: &[],
        about: &[
            "With no argument, prints the current preference. With a name, makes \
             that distro the default everywhere a spec doesn't name one: \
             'jolta install 21' fetches it, and a pin like '21' prefers its builds \
             over other distros' — even higher-versioned ones. Explicit specs \
             (corretto@21) always win.",
            "The preference lives in $JOLTA_HOME/vendor; the JOLTA_VENDOR \
             environment variable overrides it per-shell. --unset returns to the \
             built-in default (temurin). Current default: {default}.",
        ],
        examples: &[
            ("jolta vendor", "show the current preference"),
            ("jolta vendor corretto", "prefer Amazon Corretto"),
            ("jolta vendor --unset", "back to temurin"),
        ],
    },
    HelpPage {
        name: "list",
        summary: "list installed JDKs and where they come from",
        usage: &["jolta list [--json]"],
        aliases: &["ls"],
        about: &[
            "Shows every JDK jolta can use: jolta-managed installs in \
             $JOLTA_HOME/jdks and system installs discovered from Homebrew, \
             /Library/Java, SDKMAN, and friends.",
            "The * marks the JDK active in the current directory, and the footer \
             says which pin selected it.",
            "--json emits the same data for tooling: the pin plus one object per \
             JDK (version, major, vendor, home, managed, active).",
        ],
        examples: &[
            ("jolta list", ""),
            ("jolta list --json | jq -r '.jdks[].home'", "for scripts and editors"),
        ],
    },
    HelpPage {
        name: "catalog",
        summary: "browse the JDK catalog",
        usage: &["jolta catalog [<major> | <distro>[@<version>]] [--fresh]"],
        aliases: &["search", "available", "ls-remote"],
        about: &[
            "With no argument, shows each distro's newest release. Pass a major \
             (21) to compare what every distro publishes for it, or a distro \
             (temurin) for its latest build per major (the LTS releases plus the \
             current one).",
            "A @<version> suffix is a prefix filter: temurin@21 lists every \
             published 21.x build, temurin@21.0 narrows to 21.0.x. Installed \
             builds are marked.",
            "Results are cached for 24 hours (JOLTA_CACHE_TTL_HOURS tunes this); \
             --fresh refetches.",
        ],
        examples: &[
            ("jolta catalog", "newest release per distro"),
            ("jolta catalog 21", "every distro's latest 21"),
            ("jolta catalog temurin", "temurin's latest per major"),
            ("jolta catalog temurin@21.0", "all published 21.0.x builds"),
        ],
    },
    HelpPage {
        name: "jdks",
        summary: "machine-readable list of installed JDKs",
        usage: &["jolta jdks"],
        aliases: &[],
        about: &[
            "One line per installed JDK: major, full version, distro, and home \
             directory, separated by tabs. Stable output meant for scripts and \
             CI — nothing is colored or aligned.",
        ],
        examples: &[
            ("jolta jdks", "everything, as TSV"),
            ("jolta jdks | awk -F'\\t' '$1==21 {print $4}'", "homes of installed 21s"),
        ],
    },
    HelpPage {
        name: "current",
        summary: "show the Java version resolved here",
        usage: &["jolta current [--json]"],
        aliases: &[],
        about: &[
            "Prints the version, distro, and home of the JDK a java command would \
             use in the current directory — plus which pin selected it (a \
             project's .java-version, the global default, or the system JDK). \
             --json emits the same as one object.",
        ],
        examples: &[
            ("jolta current", ""),
            ("jolta current --json", "{\"version\": ..., \"home\": ...}"),
        ],
    },
    HelpPage {
        name: "which",
        summary: "show the real binary a shim would exec",
        usage: &["jolta which [<tool>]"],
        aliases: &[],
        about: &[
            "Prints the full path inside the resolved JDK that the shim would exec \
             for <tool> (default: java). Useful to double-check what a build is \
             really running, or to hand a tool path to an IDE.",
        ],
        examples: &[
            ("jolta which", "path of the resolved java"),
            ("jolta which javac", "any JDK tool works"),
        ],
    },
    HelpPage {
        name: "exec",
        summary: "run a command against the project's JDK",
        usage: &["jolta exec <command> [args...]"],
        aliases: &[],
        about: &[
            "Runs any command with JAVA_HOME set and the resolved JDK's bin \
             directory first on PATH. Handy for tools that aren't shimmed, or for \
             one-off runs from scripts.",
        ],
        examples: &[
            ("jolta exec mvn -version", "maven under the project's JDK"),
            ("jolta exec ./gradlew build", "gradle too"),
        ],
    },
    HelpPage {
        name: "env",
        summary: "print export statements for scripts",
        usage: &["jolta env"],
        aliases: &[],
        about: &[
            "Prints 'export JAVA_HOME=...' plus a PATH export that puts the \
             resolved JDK's bin directory first, quoted safely for eval. For \
             shell scripts and CI steps that want the project's JDK without \
             installing shims.",
        ],
        examples: &[("eval \"$(jolta env)\"", "activate in the current script")],
    },
    HelpPage {
        name: "home",
        summary: "print the resolved JAVA_HOME",
        usage: &["jolta home"],
        aliases: &[],
        about: &[
            "Prints the home directory of the JDK resolved for the current \
             directory — the value the shell hook exports as JAVA_HOME. Exits \
             non-zero when nothing resolves.",
        ],
        examples: &[
            ("jolta home", ""),
            ("export JAVA_HOME=\"$(jolta home)\"", "one-shot, without the hook"),
        ],
    },
    HelpPage {
        name: "hook",
        summary: "print the shell hook that keeps JAVA_HOME fresh",
        usage: &["jolta hook [zsh|bash|fish|powershell]"],
        aliases: &[],
        about: &[
            "Prints hook code for your shell (default: the one you're running). \
             The hook re-exports JAVA_HOME when you cd, when the pin file changes \
             under you (a git checkout or an edit — no cd needed), and when an \
             install, upgrade, or default from another shell changes what resolves. \
             Each prompt costs a builtin file read; jolta runs only on a real change.",
            "'jolta setup' adds the eval line to your profile automatically — you \
             only need this command for custom profiles or other shells.",
        ],
        examples: &[("eval \"$(jolta hook zsh)\"", "activate in the current shell")],
    },
    HelpPage {
        name: "completions",
        summary: "print shell completions",
        usage: &["jolta completions [zsh|bash|fish]"],
        aliases: &[],
        about: &[
            "Prints a completion script for your shell (default: the one you're \
             running). Commands, distros, and flags are generated from the same \
             tables the binary runs on, and uninstall/upgrade/prune complete \
             against what is actually installed.",
            "zsh: save it as _jolta on your fpath, or add \
             eval \"$(jolta completions zsh)\" to ~/.zshrc after compinit. \
             bash: add eval \"$(jolta completions bash)\" to ~/.bashrc. \
             fish: write it to ~/.config/fish/completions/jolta.fish.",
        ],
        examples: &[
            ("eval \"$(jolta completions zsh)\"", "activate in the current shell"),
            ("jolta completions fish > ~/.config/fish/completions/jolta.fish", ""),
        ],
    },
    HelpPage {
        name: "toolchains",
        summary: "Maven toolchains.xml from installed JDKs",
        usage: &["jolta toolchains [--write]"],
        aliases: &[],
        about: &[
            "Maven's toolchain resolution (and Gradle's auto-provisioning) \
             discovers or downloads JDKs on its own, silently bypassing jolta. \
             This command hands both build tools the jolta-managed set instead: \
             one <toolchain> entry per distro+major, newest build wins.",
            "Prints the XML to stdout by default. --write manages \
             ~/.m2/toolchains.xml directly — it refuses to overwrite a file it \
             didn't generate, so a hand-written toolchains.xml is never lost. \
             Re-run it after installing or removing JDKs.",
            "For Gradle, the matching org.gradle.java.installations.paths line \
             for ~/.gradle/gradle.properties is printed alongside, plus \
             auto-download=false so Gradle's resolver plugins stop fetching \
             their own JDKs.",
        ],
        examples: &[
            ("jolta toolchains", "print, e.g. to merge by hand"),
            ("jolta toolchains --write", "manage ~/.m2/toolchains.xml"),
        ],
    },
    HelpPage {
        name: "mirror",
        summary: "build or verify an offline JDK mirror",
        usage: &[
            "jolta mirror sync <dir> [--from <base>] [--vendors a,b] [--majors 21,17]",
            "jolta mirror verify <dir>",
        ],
        aliases: &[],
        about: &[
            "sync downloads JDK assets for every platform, .sha256 sidecars, and \
             the metadata files jolta reads (latest, index.txt, lts) into <dir>. \
             Serve that directory and point JOLTA_DOWNLOAD_BASE at it: installs, \
             upgrades, and the catalog then work without vendor access.",
            "--vendors and --majors narrow what is fetched; --from mirrors from \
             another mirror instead of the vendors. verify re-hashes every asset \
             against its .sha256 sidecar.",
        ],
        examples: &[
            ("jolta mirror sync ./m --vendors temurin --majors 17,21", "small mirror"),
            ("jolta mirror verify ./m", "check integrity"),
            ("JOLTA_DOWNLOAD_BASE=https://mirror.corp/jdk jolta install 21", "use it"),
        ],
    },
    HelpPage {
        name: "reshim",
        summary: "regenerate shims from installed JDKs",
        usage: &["jolta reshim"],
        aliases: &[],
        about: &[
            "Rebuilds $JOLTA_HOME/shims from scratch: the baseline JDK tool set, \
             plus any extra tools your installed JDKs ship (GraalVM's \
             native-image, vendor-specific extras, ...).",
            "install, uninstall, upgrade, and setup already reshim for you — reach \
             for this after adding a system JDK with new tools, or if the shims \
             directory was damaged.",
        ],
        examples: &[("jolta reshim", "")],
    },
    HelpPage {
        name: "doctor",
        summary: "diagnose common setup problems",
        usage: &["jolta doctor [--fix]"],
        aliases: &[],
        about: &[
            "Checks the whole chain: the install link, the shims, PATH order, what \
             'java' actually resolves to, JAVA_HOME freshness, ~/.mavenrc \
             overrides, the resolved java's CPU architecture, and mirror metadata \
             when JOLTA_DOWNLOAD_BASE is set.",
            "Exits non-zero when something is broken, so it can gate CI or \
             provisioning scripts.",
            "--fix repairs what is mechanically safe: rebuilds broken or missing \
             shims and re-adds missing shell-profile blocks. Anything needing \
             judgment (stale JAVA_HOME, mavenrc overrides, a dangling Homebrew \
             link) is diagnosed only.",
        ],
        examples: &[
            ("jolta doctor", ""),
            ("jolta doctor --fix", "diagnose, then repair the safe parts"),
        ],
    },
    HelpPage {
        name: "implode",
        summary: "uninstall jolta completely",
        usage: &["jolta implode [--yes]"],
        aliases: &[],
        about: &[
            "Removes $JOLTA_HOME — including every jolta-installed JDK — and \
             strips jolta's lines from your shell profile. JDKs installed outside \
             jolta (Homebrew, /Library/Java, SDKMAN, ...) are untouched.",
            "Shows exactly what will be deleted and asks for confirmation first; \
             --yes skips the prompt for scripted removal.",
        ],
        examples: &[("jolta implode", "")],
    },
    HelpPage {
        name: "version",
        summary: "print jolta's version",
        usage: &["jolta version"],
        aliases: &["-v", "--version"],
        about: &[],
        examples: &[("jolta version", "")],
    },
];

pub fn cmd_help(topic: &str) {
    let canonical = match topic {
        "search" | "available" | "ls-remote" => "catalog",
        "ls" => "list",
        "outdated" => "update",
        "-v" | "--version" => "version",
        "help" | "-h" | "--help" => {
            usage();
            return;
        }
        t => t,
    };
    let Some(page) = PAGES.iter().find(|p| p.name == canonical) else {
        die(&format!("no help page for '{topic}' — run 'jolta help' for the command list"));
    };
    let fill = |s: &str| {
        s.replace("{default}", default_vendor())
            .replace("{distros}", &INSTALLABLE_VENDORS.join(", "))
    };

    println!();
    println!("{} {} {}", bold("jolta"), bold(&cyan(page.name)), dim(&format!("— {}", page.summary)));
    println!();
    for (i, u) in page.usage.iter().enumerate() {
        // pad BEFORE painting: ANSI escapes would break {:<9} alignment
        let label = if i == 0 { "Usage:" } else { "" };
        println!("  {} {}", bold(&format!("{label:<9}")), cyan(&fill(u)));
    }
    if !page.aliases.is_empty() {
        println!("  {} {}", bold(&format!("{:<9}", "Aliases:")), page.aliases.join(", "));
    }
    for para in page.about {
        println!();
        for line in wrap(&fill(para), 72) {
            println!("  {line}");
        }
    }
    if !page.examples.is_empty() {
        println!();
        println!("  {}", bold("Examples"));
        println!();
        let w = page.examples.iter().map(|(c, _)| c.len()).max().unwrap_or(0);
        for (c, note) in page.examples {
            if note.is_empty() {
                println!("    {} {}", dim("$"), cyan(c));
            } else {
                println!("    {} {}  {}", dim("$"), cyan(&format!("{c:<w$}")), dim(note));
            }
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::PAGES;

    /// Every command main.rs dispatches needs a help page — keep this list in
    /// step with the dispatch match when adding a command.
    #[test]
    fn every_command_has_a_help_page() {
        const COMMANDS: [&str; 25] = [
            "setup", "pin", "default", "install", "update", "upgrade", "uninstall", "prune",
            "vendor", "list", "catalog", "jdks", "current", "which", "exec", "env", "home",
            "hook", "completions", "toolchains", "mirror", "reshim", "doctor", "implode",
            "version",
        ];
        for c in COMMANDS {
            assert!(PAGES.iter().any(|p| p.name == c), "'{c}' has no help page");
        }
    }

    /// The docs' JSON-LD softwareVersion must move with Cargo.toml — CI is
    /// the release gate, so a version bump that forgets the docs fails here.
    #[test]
    fn docs_version_matches_cargo() {
        let want = format!("\"softwareVersion\": \"{}\"", env!("CARGO_PKG_VERSION"));
        for (name, html) in [
            ("docs/index.html", include_str!("../docs/index.html")),
            ("docs/manual.html", include_str!("../docs/manual.html")),
        ] {
            assert!(html.contains(&want), "{name} softwareVersion != {}", env!("CARGO_PKG_VERSION"));
        }
    }

    #[test]
    fn page_names_are_unique() {
        let mut names: Vec<&str> = PAGES.iter().map(|p| p.name).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(n, names.len(), "duplicate help page names");
    }
}

/// Greedy word-wrap for the about paragraphs (plain text, no ANSI).
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}
