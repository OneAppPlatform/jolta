# Jolta

**Like [Volta](https://volta.sh), but for Java.** Pin a JDK version per project and never
manually switch again — `java`, `javac`, `mvn`-launched builds, everything just uses the
right JDK for whatever directory you're in.

```sh
cd my-service
jolta pin 21          # writes .java-version
java -version         # → OpenJDK 21, automatically

cd ../legacy-app      # has .java-version with "8"
java -version         # → 1.8, no sdk use / jenv shell / export JAVA_HOME
```

No shell hooks, no `cd` interception. Jolta puts lightweight **shims** on your `PATH`
(`java`, `javac`, `jar`, `jshell`, …). Each invocation walks up from the current
directory to find the nearest `.java-version`, resolves an installed JDK for it, sets
`JAVA_HOME`, and execs the real binary. Overhead is a few milliseconds.

> **This is the Rust implementation** (single static binary, shims are symlinks to
> the binary itself — argv[0] dispatch, ~2ms resolution). The reference POSIX sh
> implementation lives on the `main` branch; `test/smoke.sh` is the shared
> conformance suite and both implementations pass it.

## Install

Requirements: macOS or Linux, zsh or bash, `curl` (for JDK auto-install), and a
Rust toolchain to build (until prebuilt binaries are published).

One-liner (no clone left behind — fetches a tarball to a temp dir, builds,
installs into `~/.jolta`, and cleans up):

```sh
curl -fsSL https://raw.githubusercontent.com/dave-oneapp/jolta/rust/install.sh | sh
```

> While this repo is private, the raw URL needs auth — either run
> `gh api repos/dave-oneapp/jolta/contents/install.sh?ref=rust -H "Accept: application/vnd.github.raw" | sh`
> (the installer itself falls back to `gh` for the tarball too), or use the clone route:

```sh
git clone -b rust https://github.com/dave-oneapp/jolta.git && cd jolta
cargo build --release && ./target/release/jolta setup
```

`setup` installs a self-contained copy of jolta into `~/.jolta` (you can delete the
clone afterward), generates shims for every JDK it can find, and appends two small
marked blocks to your shell profile: one putting `~/.jolta/shims` on your `PATH`, one
enabling the `JAVA_HOME` cd hook. Open a new shell and run `jolta doctor` to verify.
Re-running `./bin/jolta setup` from a newer checkout upgrades the installed copy.

## Uninstall

```sh
jolta implode
```

Removes `~/.jolta` (including any downloaded JDKs) and the jolta lines from your
shell profile, after a confirmation prompt. Your `.java-version` files and any
Homebrew/system JDKs are untouched.

## Pinning

The pin lives in a plain **`.java-version`** file (compatible with jenv and asdf) —
commit it to the repo. A pin is usually just a major version:

```sh
jolta pin 21        # this project uses Java 21
jolta default 21    # global fallback when a project has no pin
```

Resolution order: `JOLTA_JAVA_VERSION` env var → nearest `.java-version` walking up →
`jolta default` → system default JDK.

Version matching is by major version, preferring an exact full-version match, then the
highest installed build of that major. `8`, `1.8`, `21`, `21.0.4`, and `temurin-21`
all parse.

## Where JDKs come from

Jolta finds JDKs you already have (Homebrew, `/Library/Java/JavaVirtualMachines`,
SDKMAN, `/usr/lib/jvm`) via `/usr/libexec/java_home` on macOS, and — like Volta —
**downloads missing ones on demand**: if a project pins a version you don't have,
the first `java`/`javac` invocation (or `jolta pin`/`exec`/`env`) fetches the
matching Eclipse Temurin build from Adoptium into `~/.jolta/jdks` and carries on.
Auto-install never triggers from the cd hook (changing directories won't start a
download), is safe under parallel builds (concurrent installs are serialized by a
lock), and can be disabled with `JOLTA_NO_AUTO_INSTALL=1`.

```sh
jolta list          # everything jolta can see, with the active one starred
jolta install 21    # explicitly download Temurin 21 from Adoptium
```

## Commands

| Command | What it does |
|---|---|
| `jolta setup` | Install shims + shell profile setup |
| `jolta pin <v>` | Write `.java-version` in the current directory |
| `jolta default <v>` | Set the global fallback version |
| `jolta install <major>` | Download a Temurin JDK from Adoptium |
| `jolta uninstall <name>` | Remove a jolta-managed JDK |
| `jolta list` | List visible JDKs, star the active one |
| `jolta current` | Show the version resolved here, and why |
| `jolta which [tool]` | Full path the shim would exec |
| `jolta exec <cmd>` | Run any command with `JAVA_HOME`/`PATH` set for this project |
| `jolta env` | Print `export` lines for `eval "$(jolta env)"` |
| `jolta home` | Print the resolved `JAVA_HOME` for this directory |
| `jolta hook [zsh\|bash]` | Print the cd hook that keeps `JAVA_HOME` in sync |
| `jolta reshim` | Regenerate shims after installing JDKs outside jolta |
| `jolta doctor` | Diagnose PATH/shim/`JAVA_HOME` problems |
| `jolta implode` | Uninstall jolta completely |

## JAVA_HOME, Maven & Gradle

Two mechanisms keep `JAVA_HOME` correct:

1. **Shims** export `JAVA_HOME` for everything they exec — any process started
   through `java`, `javac`, etc. sees the right value.
2. **The shell hook** (installed by `jolta setup`, via `eval "$(jolta hook zsh)"`)
   re-resolves `JAVA_HOME` in your interactive shell every time you `cd`. This covers
   tools that read `JAVA_HOME` directly instead of running `java` from `PATH` —
   Maven, Gradle, IDEs launched from a terminal. If a pin can't be satisfied, the
   hook unsets `JAVA_HOME` so builds fail loudly through the shim instead of silently
   using the wrong JDK.

Don't `export JAVA_HOME` manually in your profile — the hook owns it. In scripts and
CI (where the hook isn't loaded), use `jolta exec mvn ...` or `eval "$(jolta env)"`.
`jolta doctor` checks that `JAVA_HOME` matches the pin for the current directory.

## Notes & limits

- Written in dependency-free Rust (std only; downloads shell out to `curl`/`tar`).
  One binary is both the CLI and every shim.
- `jolta install` fetches the latest GA Temurin for a **major** version (aarch64/x64).
- Shims are regenerated from the union of all installed JDKs' `bin/` dirs; run
  `jolta reshim` after installing a JDK by other means (e.g. `brew install openjdk@25`).
- Resolution results are cached in `~/.jolta/cache` (invalidated automatically when the
  cached JDK disappears, and cleared by `install`/`uninstall`/`reshim`).

## Test

```sh
cargo build --release && ./test/smoke.sh
```

Runs the conformance suite against an isolated `JOLTA_HOME` in a temp dir; never
touches `~/.jolta` or your shell profile. `JOLTA_TEST_NETWORK=1` additionally
exercises real auto-install from Adoptium; `JOLTA_BIN=<path>` points the suite at a
different binary (it drives the sh implementation on `main` too).
