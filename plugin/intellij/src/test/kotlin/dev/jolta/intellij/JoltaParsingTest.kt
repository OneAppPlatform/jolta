package dev.jolta.intellij

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files

/**
 * Parsing and version-arithmetic checks for the jolta CLI boundary.
 *
 * Transposed from the failure modes of the tools this plugin competes with:
 *
 *   - intellij-mise #366  WebStorm 2025.2.5 startup crash: the CLI's JSON grew
 *                         a shape the plugin's deserializer refused, and SDK
 *                         detection died with it.
 *   - intellij-mise #387  "Plugin fails to detect aliased tool" — same class,
 *                         CLI output drifting under a pinned plugin.
 *   - intellij-mise #120  "Jackson deserialization failure for MiseDevTool with
 *                         missing optional fields".
 *   - jenv                version-file parsing and the 1.8 vs 8 legacy split.
 *
 * The invariant: a CLI that changes shape must degrade to "don't know" — never
 * throw, because every caller here runs inside a startup activity or a run
 * configuration launch.
 */
class JoltaParsingTest {
    /* ---------- A. current --json: the CLI-drift surface (mise #366/#387/#120) ---------- */

    @Test
    fun `full object parses`() {
        val c = Jolta.parseCurrent(
            """{"version": "21.0.11", "vendor": "temurin", "home": "/h/temurin-21.0.11", "source": "/p/.java-version"}""",
        )!!
        assertEquals("21.0.11", c.version)
        assertEquals("temurin", c.vendor)
        assertEquals("/h/temurin-21.0.11", c.home)
        assertEquals("/p/.java-version", c.source)
    }

    @Test
    fun `null vendor and source are tolerated`() {
        val c = Jolta.parseCurrent(
            """{"version": "17.0.19", "vendor": null, "home": "/h/x", "source": null}""",
        )!!
        assertNull(c.vendor)
        assertNull(c.source)
        assertEquals("17.0.19", c.version)
    }

    @Test
    fun `unknown future fields do not break parsing`() {
        val c = Jolta.parseCurrent(
            """{"version": "25.0.3", "vendor": "temurin", "home": "/h/x", "source": "s",
               "arch": "aarch64", "lts": true, "installed_at": 1234}""",
        )!!
        assertEquals("25.0.3", c.version)
    }

    @Test
    fun `missing required fields yield null rather than a partial object`() {
        assertNull(Jolta.parseCurrent("""{"vendor": "temurin", "home": "/h/x"}"""))
        assertNull(Jolta.parseCurrent("""{"version": "21", "vendor": "temurin"}"""))
        assertNull(Jolta.parseCurrent("{}"))
    }

    @Test
    fun `blank required fields are treated as missing`() {
        assertNull(Jolta.parseCurrent("""{"version": "", "home": "/h/x"}"""))
        assertNull(Jolta.parseCurrent("""{"version": "21", "home": "   "}"""))
    }

    @Test
    fun `malformed output never throws`() {
        // A CLI that panics mid-write, prints a warning banner, or is replaced
        // by something else entirely must not take the IDE down with it.
        listOf(
            "",
            "   ",
            "not json at all",
            "{",
            """{"version": "21", "home": """,
            "jolta: error: no installed JDK matches '21'",
            "[]",
            "null",
            """{"version": ["21"], "home": {"a": 1}}""",
        ).forEach { assertNull("should not parse: $it", Jolta.parseCurrent(it)) }
    }

    /* ---------- B. jdks TSV: input for stale-SDK repair ---------- */

    @Test
    fun `well formed rows parse`() {
        val rows = Jolta.parseJdks(
            """
            11	11.0.32	corretto	/h/corretto-11.0.32
            21	21.0.11	temurin	/h/temurin-21.0.11
            """.trimIndent(),
        )
        assertEquals(2, rows.size)
        assertEquals(11, rows[0].major)
        assertEquals("/h/temurin-21.0.11", rows[1].home)
    }

    @Test
    fun `short rows blank fields and non-numeric majors are skipped not fatal`() {
        val rows = Jolta.parseJdks(
            """
            21	21.0.11	temurin	/h/ok

            garbage
            xx	21.0.11	temurin	/h/bad-major
            21	21.0.11	temurin
            21		temurin	/h/blank-version
            21	21.0.11	temurin
            """.trimIndent(),
        )
        assertEquals(1, rows.size)
        assertEquals("/h/ok", rows[0].home)
    }

    @Test
    fun `paths containing spaces survive`() {
        val rows = Jolta.parseJdks("21\t21.0.11\ttemurin\t/Users/a b/.jolta/jdks/temurin-21.0.11")
        assertEquals("/Users/a b/.jolta/jdks/temurin-21.0.11", rows[0].home)
    }

    /* ---------- C. major arithmetic (jenv: the 1.8 vs 8 split) ---------- */

    @Test
    fun `modern versions report their leading major`() {
        assertEquals(21, Jolta.majorOf("21.0.11"))
        assertEquals(25, Jolta.majorOf("25"))
        assertEquals(17, Jolta.majorOf("17.0.19+7"))
        assertEquals(11, Jolta.majorOf("11.0.32"))
    }

    @Test
    fun `legacy 1_8 style resolves to 8 not 1`() {
        assertEquals(8, Jolta.majorOf("1.8.0_452"))
        assertEquals(8, Jolta.majorOf("1.8"))
        assertEquals(7, Jolta.majorOf("1.7.0_80"))
    }

    @Test
    fun `unparseable versions yield null`() {
        assertNull(Jolta.majorOf(""))
        assertNull(Jolta.majorOf("temurin"))
        assertNull(Jolta.majorOf("-21"))
    }

    /* ---------- D. governance: whose project is this? ---------- */
    /*
     * intellij-mise #393 "Automatic SDK configuration without mise usage" and
     * #98 "Skip automatic SDK configuration for Projects which are not using
     * mise.toml files": the plugin seized the SDK of projects that had nothing
     * to do with the tool. `jolta current` always answers — with no pin and no
     * default it reports whatever system JDK it found — so the plugin, not the
     * CLI, has to decide whether jolta is actually in charge here.
     */

    @Test
    fun `pin file is found by walking up (jenv walk-up semantics)`() {
        val root = Files.createTempDirectory("jolta-gov").toFile()
        try {
            val deep = File(root, "a/b/c").apply { mkdirs() }
            assertTrue("no pin anywhere yet", !Jolta.pinFileExists(deep))

            File(root, "a/.java-version").writeText("21\n")
            assertTrue("pin two levels up is found", Jolta.pinFileExists(deep))
            assertTrue("pin in its own dir is found", Jolta.pinFileExists(File(root, "a")))
            assertTrue("sibling above the pin is not governed", !Jolta.pinFileExists(root))
        } finally {
            root.deleteRecursively()
        }
    }

    @Test
    fun `sdkmanrc also counts as a pin`() {
        val root = Files.createTempDirectory("jolta-gov").toFile()
        try {
            val deep = File(root, "x/y").apply { mkdirs() }
            File(root, "x/.sdkmanrc").writeText("java=21.0.2-tem\n")
            assertTrue(Jolta.pinFileExists(deep))
        } finally {
            root.deleteRecursively()
        }
    }

    @Test
    fun `a directory named like a pin file is not a pin`() {
        val root = Files.createTempDirectory("jolta-gov").toFile()
        try {
            File(root, ".java-version").mkdirs()
            assertTrue("a directory is not a pin file", !Jolta.pinFileExists(root))
        } finally {
            root.deleteRecursively()
        }
    }
}
