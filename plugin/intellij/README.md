# Jolta for IntelliJ

Hands-off JDK selection in IntelliJ IDEA from the project's `.java-version`,
powered by the [jolta](https://oneappplatform.github.io/jolta/) CLI.

What it does, on project open and continuously after:

- Resolves the project's pin by asking jolta itself (`jolta current --json`) —
  the plugin contains zero resolution logic of its own, so the IDE, the shims,
  and CI can never disagree.
- Registers the resolved JDK in the SDK table as `jolta <vendor> <major>` and
  sets it as the **project SDK**, the **Gradle JVM**, and the **Maven importer
  JDK**.
- Watches `.java-version`/`.sdkmanrc` and jolta's state stamp: `jolta pin 25`
  in any terminal switches the IDE within seconds. `jolta upgrade` repoints
  the SDK entry instead of leaving it dangling.
- If the pin resolves to nothing installed, installs it in the background
  (only when a pin file exists — never a surprise download).

Requires the jolta CLI on the machine (`brew install OneAppPlatform/tap/jolta`);
the binary is found at `~/.jolta/bin` (or `$JOLTA_HOME/bin`), not via `PATH`,
so IDEs launched from the Dock work.

## Development

```sh
./gradlew buildPlugin        # produces build/distributions/jolta-intellij-*.zip
./gradlew runIde             # launches a sandboxed IDEA with the plugin
./gradlew verifyPlugin       # JetBrains plugin verifier
```

Install a local build via Settings → Plugins → ⚙ → Install Plugin from Disk,
pointing at the zip.
