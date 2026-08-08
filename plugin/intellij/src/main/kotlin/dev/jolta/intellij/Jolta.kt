package dev.jolta.intellij

import com.google.gson.JsonParser
import com.intellij.openapi.diagnostic.Logger
import java.io.File
import java.util.concurrent.TimeUnit

/** What `jolta current --json` said for a directory. */
data class JoltaCurrent(val version: String, val vendor: String?, val home: String, val source: String?)

/** One row of `jolta jdks`: major, full version, distro, home. */
data class JoltaJdk(val major: Int, val version: String, val vendor: String, val home: String)

/**
 * Thin wrapper around the jolta CLI. The plugin never re-implements
 * resolution — jolta is the source of truth; this just asks it.
 */
object Jolta {
    private val log = Logger.getInstance(Jolta::class.java)

    private val windows = System.getProperty("os.name", "").startsWith("Windows")

    /**
     * Test seam. Without it the suite reads the developer's real ~/.jolta and
     * a machine with a global default makes every project look governed — the
     * same hermeticity the CLI's edge suite gets from an isolated JOLTA_HOME.
     */
    @Volatile
    internal var homeOverride: File? = null

    fun joltaHome(): File {
        homeOverride?.let { return it }
        System.getenv("JOLTA_HOME")?.takeIf { it.isNotBlank() }?.let { return File(it) }
        return File(System.getProperty("user.home"), ".jolta")
    }

    /**
     * The binary is looked up at $JOLTA_HOME/bin, never via PATH — an IDE
     * launched from the Dock/Finder doesn't inherit the shell's PATH.
     */
    fun binary(): File? {
        val f = File(File(joltaHome(), "bin"), if (windows) "jolta.exe" else "jolta")
        return if (f.canExecute()) f else null
    }

    /** Every mutating jolta command bumps this; watching it keeps the IDE live. */
    fun stampFile(): File = File(joltaHome(), ".stamp")

    /** Outcome of a jolta invocation. [rc] is null when it never ran or timed out. */
    data class Exec(val rc: Int?, val stdout: String, val stderr: String) {
        val ok: Boolean get() = rc == 0

        /**
         * The most useful line to show a human. jolta writes diagnostics to
         * stderr prefixed "jolta:"; falling back to stdout keeps us from
         * reporting an empty failure.
         */
        fun message(): String =
            stderr.lineSequence().map { it.trim() }.filter { it.isNotEmpty() }.lastOrNull()
                ?: stdout.lineSequence().map { it.trim() }.lastOrNull { it.isNotEmpty() }
                ?: "jolta exited with ${rc ?: "no status"}"
    }

    fun exec(workDir: File?, timeoutSec: Long, vararg args: String): Exec {
        val bin = binary() ?: return Exec(null, "", "jolta is not installed at ${joltaHome()}/bin")
        return try {
            val p = ProcessBuilder(listOf(bin.absolutePath) + args)
                .directory(workDir)
                .start()
            p.outputStream.close()
            // Both pipes must be drained concurrently — a child that fills the
            // stderr buffer while we block on stdout deadlocks.
            val err = StringBuilder()
            val errPump = Thread { p.errorStream.bufferedReader().forEachLine { err.appendLine(it) } }
            errPump.isDaemon = true
            errPump.start()

            val out = p.inputStream.bufferedReader().readText()
            if (!p.waitFor(timeoutSec, TimeUnit.SECONDS)) {
                p.destroyForcibly()
                log.warn("jolta ${args.joinToString(" ")} timed out after ${timeoutSec}s")
                return Exec(null, out, "jolta ${args.first()} timed out after ${timeoutSec}s")
            }
            errPump.join(1000)
            Exec(p.exitValue(), out, err.toString())
        } catch (e: Exception) {
            log.warn("jolta ${args.joinToString(" ")} failed", e)
            Exec(null, "", e.message ?: e.javaClass.simpleName)
        }
    }

    fun run(workDir: File?, timeoutSec: Long, vararg args: String): String? =
        exec(workDir, timeoutSec, *args).takeIf { it.ok }?.stdout

    /**
     * Write `.java-version` in [projectDir]. jolta fetches the JDK when nothing
     * installed matches, so this can download — hence the generous timeout.
     *
     * [resolved] pins the exact point release the spec lands on rather than the
     * major, which is what a team wanting reproducible CI asks for.
     */
    fun pin(projectDir: File, spec: String, resolved: Boolean = false): Exec {
        val args = mutableListOf("pin", spec)
        if (resolved) args += "--resolved"
        return exec(projectDir, 600, *args.toTypedArray())
    }

    fun current(projectDir: File, timeoutSec: Long = 30): JoltaCurrent? =
        run(projectDir, timeoutSec, "current", "--json")?.let(::parseCurrent)

    /**
     * Parsing is separated from spawning so it can be tested, and so a CLI
     * that grows, drops, or nulls a field degrades to "don't know" instead of
     * throwing into a startup activity.
     */
    fun parseCurrent(out: String): JoltaCurrent? =
        try {
            val o = JsonParser.parseString(out).asJsonObject
            fun str(k: String): String? =
                o.get(k)?.takeIf { !it.isJsonNull && it.isJsonPrimitive }?.asString?.takeIf { it.isNotBlank() }
            val version = str("version")
            val home = str("home")
            if (version == null || home == null) {
                log.warn("jolta current --json lacks version/home — ignoring")
                null
            } else {
                JoltaCurrent(version, str("vendor"), home, str("source"))
            }
        } catch (e: Exception) {
            log.warn("cannot parse jolta current --json output", e)
            null
        }

    fun jdks(): List<JoltaJdk> {
        val out = run(null, 30, "jdks") ?: return emptyList()
        return parseJdks(out)
    }

    /** Tab-separated `major\tversion\tvendor\thome`; unparseable rows are skipped. */
    fun parseJdks(out: String): List<JoltaJdk> =
        out.lineSequence().mapNotNull { line ->
            val f = line.split('\t')
            if (f.size < 4) return@mapNotNull null
            val major = f[0].trim().toIntOrNull() ?: return@mapNotNull null
            if (f[1].isBlank() || f[3].isBlank()) return@mapNotNull null
            JoltaJdk(major, f[1], f[2], f[3])
        }.toList()

    /**
     * Resolve with auto-install: `jolta env` downloads the pinned JDK when
     * nothing installed satisfies it. Generous timeout — this can download.
     */
    fun resolveInstalling(projectDir: File): Boolean =
        run(projectDir, 600, "env") != null

    /** Is there a pin file at or above [dir]? Auto-install is only offered then. */
    fun pinFileExists(dir: File): Boolean {
        var d: File? = dir
        while (d != null) {
            if (File(d, ".java-version").isFile || File(d, ".sdkmanrc").isFile) return true
            d = d.parentFile
        }
        return false
    }

    /**
     * The spec to write into `.java-version` for a given JDK.
     *
     * Vendor-qualified when we know the vendor (`temurin@21`), bare major
     * otherwise. The major rather than the point release: a pin of `21` keeps
     * matching after `jolta upgrade`, where `21.0.11` would go stale the moment
     * a patch lands. The dialog's "exact" checkbox is how you ask for the
     * stricter thing on purpose.
     */
    fun specFor(vendor: String?, major: Int): String =
        if (vendor.isNullOrBlank() || vendor == "?") "$major" else "$vendor@$major"

    /**
     * Would jolta accept this as a pin? Checked before writing rather than
     * after, because an unparseable `.java-version` breaks every java
     * invocation below that directory — including the shell the user would
     * need in order to fix it.
     */
    /**
     * Clean up what someone typed before it reaches the CLI.
     *
     * A dangling separator carries no information — `@21` and `-21` say nothing
     * a bare `21` doesn't — so trim it rather than refusing the pin over
     * punctuation. This is deliberately narrow: it only drops separators with
     * nothing meaningful attached. `notavendor@21` is left alone, because
     * silently discarding a vendor someone asked for would pin them to a JDK
     * they didn't choose.
     */
    fun normalizeSpec(spec: String): String =
        spec.trim().trim('@', '-').trim()

    fun isPinnable(spec: String): Boolean {
        val s = normalizeSpec(spec)
        if (s.isEmpty()) return false
        if (s.equals("lts", true) || s.equals("latest", true)) return true
        return majorOf(versionPartOf(s)) != null
    }

    /**
     * The version half of a spec, split exactly the way the CLI's `parse_spec`
     * does: a separator only counts when what precedes it is a vendor jolta
     * knows. Splitting on any `@` would call "@21" pinnable — the CLI refuses
     * it — and splitting on any `-` would mangle "21-ea".
     */
    private fun versionPartOf(spec: String): String {
        for (sep in charArrayOf('@', '-')) {
            val i = spec.indexOf(sep)
            if (i > 0 && spec.take(i).lowercase() in KNOWN_VENDORS) return spec.substring(i + 1)
        }
        return spec
    }

    /** Mirrors KNOWN_VENDORS in src/jdk.rs. */
    private val KNOWN_VENDORS = setOf(
        "temurin", "corretto", "graalvm", "oracle", "zulu", "liberica", "sapmachine", "graalce",
        "openjdk", "semeru", "microsoft", "dragonwell",
    )

    /** A global default is deliberate configuration: "jolta manages my Java". */
    fun defaultFileExists(): Boolean = File(joltaHome(), "default").isFile

    /**
     * Is jolta actually in charge here?
     *
     * `jolta current` always answers — with no pin and no default it reports
     * whatever system JDK it found. Acting on that would mean seizing the SDK
     * of every project the user opens, jolta-related or not, which is the same
     * overreach mise had to walk back (their #393). Governance requires a pin
     * at or above [dir], or an explicit global default.
     */
    fun isGoverned(dir: File): Boolean = pinFileExists(dir) || defaultFileExists()

    /**
     * Component-wise numeric ordering. String ordering is wrong the moment a
     * patch level reaches double digits — "21.0.9" sorts above "21.0.12" — which
     * would quietly repoint a repaired SDK at an older JDK than it had.
     */
    fun compareVersions(a: String, b: String): Int {
        val pa = a.split('.', '_', '+', '-')
        val pb = b.split('.', '_', '+', '-')
        for (i in 0 until maxOf(pa.size, pb.size)) {
            val x = pa.getOrNull(i)?.takeWhile { it.isDigit() }?.toIntOrNull() ?: 0
            val y = pb.getOrNull(i)?.takeWhile { it.isDigit() }?.toIntOrNull() ?: 0
            if (x != y) return x.compareTo(y)
        }
        return 0
    }

    /** Newest build among [candidates], by [compareVersions]. */
    fun newest(candidates: List<JoltaJdk>): JoltaJdk? =
        candidates.maxWithOrNull { a, b -> compareVersions(a.version, b.version) }

    fun majorOf(version: String): Int? {
        val first = version.takeWhile { it.isDigit() }.toIntOrNull() ?: return null
        if (first != 1) return first
        // legacy "1.8" style
        return version.split('.').getOrNull(1)?.takeWhile { it.isDigit() }?.toIntOrNull()
    }
}
