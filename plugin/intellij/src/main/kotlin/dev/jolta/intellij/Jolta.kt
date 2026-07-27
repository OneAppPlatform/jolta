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

    fun joltaHome(): File {
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

    fun run(workDir: File?, timeoutSec: Long, vararg args: String): String? {
        val bin = binary() ?: return null
        return try {
            val p = ProcessBuilder(listOf(bin.absolutePath) + args)
                .directory(workDir)
                .start()
            p.outputStream.close()
            val out = p.inputStream.bufferedReader().readText()
            p.errorStream.bufferedReader().readText() // drain so the child can't block
            if (!p.waitFor(timeoutSec, TimeUnit.SECONDS)) {
                p.destroyForcibly()
                log.warn("jolta ${args.joinToString(" ")} timed out after ${timeoutSec}s")
                return null
            }
            if (p.exitValue() == 0) out else null
        } catch (e: Exception) {
            log.warn("jolta ${args.joinToString(" ")} failed", e)
            null
        }
    }

    fun current(projectDir: File): JoltaCurrent? {
        val out = run(projectDir, 30, "current", "--json") ?: return null
        return try {
            val o = JsonParser.parseString(out).asJsonObject
            fun str(k: String): String? = o.get(k)?.takeIf { !it.isJsonNull }?.asString
            JoltaCurrent(
                version = str("version") ?: return null,
                vendor = str("vendor"),
                home = str("home") ?: return null,
                source = str("source"),
            )
        } catch (e: Exception) {
            log.warn("cannot parse jolta current --json output", e)
            null
        }
    }

    fun jdks(): List<JoltaJdk> {
        val out = run(null, 30, "jdks") ?: return emptyList()
        return out.lineSequence().mapNotNull { line ->
            val f = line.split('\t')
            if (f.size < 4) return@mapNotNull null
            val major = f[0].toIntOrNull() ?: return@mapNotNull null
            JoltaJdk(major, f[1], f[2], f[3])
        }.toList()
    }

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

    fun majorOf(version: String): Int? {
        val first = version.takeWhile { it.isDigit() }.toIntOrNull() ?: return null
        if (first != 1) return first
        // legacy "1.8" style
        return version.split('.').getOrNull(1)?.takeWhile { it.isDigit() }?.toIntOrNull()
    }
}
