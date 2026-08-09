# Jolta for IntelliJ

Automatic JDK selection in IntelliJ IDEA from the project's `.java-version`,
powered by the [jolta](https://oneappplatform.github.io/jolta/) CLI.

## Setting the version from the IDE

The plugin writes the pin, it doesn't just read it — choosing a JDK in the IDE
and having the terminal disagree is the problem jolta exists to remove.

- **Status bar** shows the active Java version. Click it to switch between
  installed JDKs, open the picker, or refresh from jolta.
- **Tools → Jolta → Set Java Version…** opens the picker: installed and
  discovered JDKs, or type any spec (`21`, `corretto@21`, `lts`, `21.0.4`) —
  jolta downloads what you don't have. "Pin the exact point release" writes the
  resolved build instead of the major, for teams that don't want CI drifting
  onto a different patch.
- **Tools → Jolta → Reload JDK** re-resolves on demand and always reports what
  it found, including "already up to date".
- Opening an unpinned project whose SDK is healthy offers to pin it to the
  version it's already using. Declining is remembered per project.

Specs are validated against the same rules the CLI uses before anything is
written: an unparseable `.java-version` would break every `java` invocation
below that directory, including the shell you'd need to fix it.

## When things disagree

The plugin distinguishes **broken** from **different**, because most differences
are deliberate:

- A Gradle **toolchain** (`java.toolchain.languageVersion = 17`) compiling with
  a JDK other than the pin is correct — the pin launches Gradle, the toolchain
  compiles. Same for `org.gradle.java.home` and Maven `toolchains.xml`. These are
  never reported.
- **Choosing a different Project SDK by hand is respected.** jolta's stamp is
  global, so any `jolta install` anywhere re-syncs every open project; without
  this, a JDK you picked in Project Structure would silently revert minutes
  later. Your choice stands until the pin itself changes — then the pin wins,
  because a new pin is new information. The status bar shows the divergence with
  a one-click **Reapply the pin**.
- Only genuinely broken states get a notification: an SDK whose directory has
  gone, a Gradle/Maven JDK the IDE doesn't have, or a `.java-version` and
  `.sdkmanrc` in the same directory that disagree (jolta uses `.java-version`,
  so the other is silently ignored). Each is reported once, with a fix.

The plugin acts only where jolta is actually in charge: a `.java-version` or
`.sdkmanrc` at or above the project, or an explicit global default. `jolta
current` always answers — with no pin at all it reports whatever system JDK it
found — so opening an unrelated project must not hand its SDK to jolta.

What it does, on project open and continuously after:

- Resolves the project's pin by asking jolta itself (`jolta current --json`) —
  the plugin contains zero resolution logic of its own, so the IDE, the shims,
  and CI can never disagree.
- Registers the resolved JDK in the SDK table as `jolta <vendor> <major>` and
  sets it as the **project SDK**, the **Gradle JVM**, and the **Maven importer
  JDK**.
- Injects `JAVA_HOME` and a leading `PATH` entry into Java run configurations,
  resolved against each configuration's own working directory. The project SDK
  covers compilation; this covers everything a run shells out to. Gradle and
  Maven run configurations are left alone — their JDK already comes from the
  Gradle JVM / importer settings above, and the only way into their environment
  is persisted state that would have to be restored after every run.
- Watches `.java-version`/`.sdkmanrc` and jolta's state stamp: `jolta pin 25`
  in any terminal switches the IDE within seconds. `jolta upgrade` repoints
  the SDK entry instead of leaving it dangling.
- If the pin resolves to nothing installed, installs it in the background
  (only when a pin file exists — never a surprise download).
- Taking over an SDK you had set by hand comes with an **Undo** on the
  notification. The pin still wins by default; it just isn't a one-way door.
- **Tools → Jolta: Reload JDK** re-resolves and reapplies on demand, and always
  reports what it found — including "already up to date".

Requires the jolta CLI on the machine (`brew install OneAppPlatform/tap/jolta`);
the binary is found at `~/.jolta/bin` (or `$JOLTA_HOME/bin`), not via `PATH`,
so IDEs launched from the Dock work.

### Windows

The CLI needs a manual install for now. The plugin detects this and links
straight to the download rather than offering a command that won't run:

1. Download `jolta-x86_64-pc-windows-msvc.zip` from the
   [releases page](https://github.com/OneAppPlatform/jolta/releases)
2. Unzip it anywhere
3. Run `.\jolta.exe setup`

Setup is first-class on Windows — it adds the shims and bin directories to your
user `PATH` in the registry (broadcasting the change so new terminals see it)
and adds the `JAVA_HOME` hook to your PowerShell profiles. `jolta implode`
undoes both. Once that's done the plugin behaves exactly as it does elsewhere;
it looks for `jolta.exe` under `%USERPROFILE%\.jolta\bin`.

Sorry for the extra step. A winget package (`OneAppPlatform.Jolta`) is submitted
and awaiting review; once it merges this becomes
`winget install OneAppPlatform.Jolta` and the plugin will offer to run it for
you, the same as the curl installer on macOS and Linux.

## Development

```sh
./gradlew test               # platform tests (see below)
./gradlew buildPlugin        # produces build/distributions/jolta-intellij-*.zip
./gradlew runIde             # launches a sandboxed IDEA with the plugin
./gradlew verifyPlugin       # JetBrains verifier, against 2024.2.4 and 2025.1.4
```

### Tests

Like `test/edge.sh` for the CLI, the suite is mined from the tools jolta
replaces — their test suites first, then their closed issues, then their open
ones. Every section names what it was transposed from, so a case can be traced
back to the bug it exists to prevent:

- `JoltaParsingTest` — the CLI boundary. A `jolta` whose JSON grows, drops, or
  nulls a field must degrade to "don't know", never throw inside a startup
  activity (intellij-mise #366, #387, #120). Plus jenv's `1.8` vs `8` split and
  pin walk-up semantics.
- `JoltaRunConfigurationExtensionTest` — environment injection: missing working
  directory, not clobbering the user's own env, idempotence across repeated
  launches, subdirectory pins (intellij-mise v0.4.x, #230, #50/#64;
  sdkman-cli #584).
- `JoltaPinningTest` — writing the pin, the plugin's one destructive act:
  spec construction, and validation checked for parity with the CLI in both
  directions (the plugin must not reject what jolta accepts, e.g. `21.x`, nor
  accept what it refuses, e.g. `@21`). Plus the error-reporting path, so a
  failed pin never surfaces an empty message.
- `JoltaHealthTest` — what counts as out of sync. Every case is a reference to a
  JDK that isn't on disk, or two files giving contradictory instructions. An SDK
  that merely differs from the pin is asserted *not* to be reported — that false
  positive is what mise had to walk back twice (#99, #354).
- `JoltaOverrideTest` — when a hand-picked SDK outranks a re-sync, and when the
  pin takes it back.
- `JoltaSdkTableTest` — SDK-table hygiene: no duplicate entries, names stable
  across point releases, dangling entries detected and repointed at the *newest*
  build (intellij-mise #370; IDEA-358716, IDEA-354569; the standing jenv advice
  to hand-delete SDKs whose directories vanished).

Install a local build via Settings → Plugins → ⚙ → Install Plugin from Disk,
pointing at the zip.
