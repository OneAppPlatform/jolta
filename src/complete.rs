//! Shell completion scripts (`jolta completions <shell>`). Generated at
//! print time from the same tables the binary runs on — help pages, vendor
//! lists, the baseline tool set — so completions never drift from reality.
//! Installed-JDK suggestions stay live by shelling out to `jolta jdks`
//! at completion time.

use crate::commands::BASELINE_TOOLS;
use crate::help::PAGES;
use crate::jdk::{INSTALLABLE_VENDORS, KNOWN_VENDORS};
use crate::ui::die;

pub fn cmd_completions(shell: &str) {
    match shell {
        "zsh" => print_zsh(),
        "bash" => print_bash(),
        "fish" => print_fish(),
        other => die(&format!("no completions for shell '{other}' (zsh, bash, and fish are supported)")),
    }
}

/// Static spec suggestions for pin/default/install: common majors, the
/// keywords, and vendor@major combos.
fn spec_words() -> Vec<String> {
    const MAJORS: [&str; 7] = ["21", "17", "25", "11", "8", "lts", "latest"];
    let mut words: Vec<String> = MAJORS.iter().map(|m| m.to_string()).collect();
    for v in INSTALLABLE_VENDORS {
        for m in MAJORS {
            words.push(format!("{v}@{m}"));
        }
    }
    words
}

fn command_names() -> String {
    PAGES.iter().map(|p| p.name).collect::<Vec<_>>().join(" ")
}

fn print_zsh() {
    println!("#compdef jolta");
    println!("_jolta() {{");
    println!("  local -a _cmds");
    println!("  _cmds=(");
    for p in PAGES {
        // zsh 'name:desc' entries; embedded ' becomes '\''
        println!("    '{}:{}'", p.name, p.summary.replace('\'', r"'\''"));
    }
    println!("  )");
    print!(
        r#"  if (( CURRENT == 2 )); then
    _describe -t commands 'jolta command' _cmds
    return
  fi
  local cmd=$words[2]
  local -a dyn
  case $cmd in
    pin|default|install)
      compadd -- {specs}
      [[ $cmd == pin ]] && compadd -- --resolved
      [[ $cmd == install ]] && compadd -- --fresh
      ;;
    uninstall|upgrade|prune)
      dyn=(${{(f)"$(jolta jdks 2>/dev/null | awk -F'\t' '{{print $3"@"$1; print $3"-"$2}}' | sort -u)"}})
      compadd -- $dyn
      [[ $cmd == prune ]] && compadd -- -n --dry-run
      ;;
    vendor)
      compadd -- {known} --unset
      ;;
    catalog|search|available|ls-remote)
      compadd -- {installable} 21 17 25 11 8 --fresh
      ;;
    which)
      compadd -- {tools}
      ;;
    exec)
      (( CURRENT == 3 )) && _command_names -e
      ;;
    hook)
      compadd -- zsh bash fish powershell
      ;;
    completions)
      compadd -- zsh bash fish
      ;;
    help)
      compadd -- {commands}
      ;;
    mirror)
      (( CURRENT == 3 )) && compadd -- sync verify
      ;;
    list|ls|current)
      compadd -- --json
      ;;
    update|outdated)
      compadd -- --fresh
      ;;
    doctor)
      compadd -- --fix
      ;;
    toolchains)
      compadd -- --write
      ;;
    implode)
      compadd -- --yes
      ;;
  esac
}}
if [ "$funcstack[1]" = "_jolta" ]; then
  _jolta "$@"
else
  compdef _jolta jolta
fi
"#,
        specs = spec_words().join(" "),
        known = KNOWN_VENDORS.join(" "),
        installable = INSTALLABLE_VENDORS.join(" "),
        tools = BASELINE_TOOLS.join(" "),
        commands = command_names(),
    );
}

fn print_bash() {
    print!(
        r#"_jolta() {{
  local cur cmd
  cur=${{COMP_WORDS[COMP_CWORD]}}
  cmd=${{COMP_WORDS[1]}}
  if [ "$COMP_CWORD" -eq 1 ]; then
    COMPREPLY=($(compgen -W "{commands}" -- "$cur"))
    return
  fi
  local words=""
  case "$cmd" in
    pin)                              words="{specs} --resolved" ;;
    default)                          words="{specs}" ;;
    install)                          words="{specs} --fresh" ;;
    uninstall|upgrade|prune)
      words="$(jolta jdks 2>/dev/null | awk -F'\t' '{{print $3"@"$1; print $3"-"$2}}' | sort -u)"
      [ "$cmd" = prune ] && words="$words -n --dry-run"
      ;;
    vendor)                           words="{known} --unset" ;;
    catalog|search|available|ls-remote) words="{installable} 21 17 25 11 8 --fresh" ;;
    which)                            words="{tools}" ;;
    hook)                             words="zsh bash fish powershell" ;;
    completions)                      words="zsh bash fish" ;;
    help)                             words="{commands}" ;;
    mirror)                           [ "$COMP_CWORD" -eq 2 ] && words="sync verify" ;;
    list|ls|current)                  words="--json" ;;
    update|outdated)                  words="--fresh" ;;
    doctor)                           words="--fix" ;;
    toolchains)                       words="--write" ;;
    implode)                          words="--yes" ;;
  esac
  COMPREPLY=($(compgen -W "$words" -- "$cur"))
}}
complete -F _jolta jolta
"#,
        commands = command_names(),
        specs = spec_words().join(" "),
        known = KNOWN_VENDORS.join(" "),
        installable = INSTALLABLE_VENDORS.join(" "),
        tools = BASELINE_TOOLS.join(" "),
    );
}

fn print_fish() {
    println!("complete -c jolta -f");
    for p in PAGES {
        println!(
            "complete -c jolta -n __fish_use_subcommand -a {} -d \"{}\"",
            p.name,
            p.summary.replace('"', "\\\"")
        );
    }
    // installed JDKs, resolved live when the user actually completes
    const INSTALLED: &str =
        "(jolta jdks 2>/dev/null | while read -l m v ven h; echo $ven@$m; echo $ven-$v; end | sort -u)";
    print!(
        r#"complete -c jolta -n '__fish_seen_subcommand_from pin default install' -a '{specs}'
complete -c jolta -n '__fish_seen_subcommand_from pin' -a --resolved -d 'pin the exact resolved version'
complete -c jolta -n '__fish_seen_subcommand_from install update outdated catalog search available ls-remote' -a --fresh -d 'refetch release metadata'
complete -c jolta -n '__fish_seen_subcommand_from uninstall upgrade prune' -a '{installed}'
complete -c jolta -n '__fish_seen_subcommand_from prune' -a '-n --dry-run' -d 'preview only'
complete -c jolta -n '__fish_seen_subcommand_from vendor' -a '{known} --unset'
complete -c jolta -n '__fish_seen_subcommand_from catalog search available ls-remote' -a '{installable} 21 17 25 11 8'
complete -c jolta -n '__fish_seen_subcommand_from which' -a '{tools}'
complete -c jolta -n '__fish_seen_subcommand_from hook' -a 'zsh bash fish powershell'
complete -c jolta -n '__fish_seen_subcommand_from completions' -a 'zsh bash fish'
complete -c jolta -n '__fish_seen_subcommand_from help' -a '{commands}'
complete -c jolta -n '__fish_seen_subcommand_from mirror' -a 'sync verify'
complete -c jolta -n '__fish_seen_subcommand_from list ls current' -a --json -d 'machine-readable output'
complete -c jolta -n '__fish_seen_subcommand_from doctor' -a --fix -d 'repair what is safe to repair'
complete -c jolta -n '__fish_seen_subcommand_from toolchains' -a --write -d 'write ~/.m2/toolchains.xml'
complete -c jolta -n '__fish_seen_subcommand_from implode' -a --yes -d 'skip confirmation'
"#,
        specs = spec_words().join(" "),
        installed = INSTALLED,
        known = KNOWN_VENDORS.join(" "),
        installable = INSTALLABLE_VENDORS.join(" "),
        tools = BASELINE_TOOLS.join(" "),
        commands = command_names(),
    );
}
