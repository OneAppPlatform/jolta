package dev.jolta.intellij

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Writing the pin — the plugin's one destructive act.
 *
 * Everything else here reads state; `jolta pin` creates a file in someone's
 * repository. The bar is correspondingly higher, and the failure modes come
 * from the same competitors:
 *
 *   - jenv         a `.java-version` the tool itself can't parse breaks every
 *                  java invocation below that directory, including the shell
 *                  you'd need to fix it. Validate before writing, not after.
 *   - sdkman-cli   `.sdkmanrc` pins the exact build; jenv pins whatever string
 *                  you gave it. Which one you want is a real choice, so the
 *                  major/exact distinction is surfaced rather than assumed.
 *   - intellij-mise #302  "Add an option to run `mise install` beforehand" — a
 *                  version you don't have yet must be a legal thing to ask for.
 */
class JoltaPinningTest {

    /* ---------- A. spec construction ---------- */

    @Test
    fun `a known vendor produces a vendor-qualified spec`() {
        assertEquals("temurin@21", Jolta.specFor("temurin", 21))
        assertEquals("corretto@11", Jolta.specFor("corretto", 11))
    }

    @Test
    fun `an unknown vendor degrades to a bare major`() {
        // `jolta jdks` prints "?" for a JDK whose vendor it couldn't identify —
        // pinning "?@21" would be nonsense.
        assertEquals("21", Jolta.specFor("?", 21))
        assertEquals("21", Jolta.specFor(null, 21))
        assertEquals("21", Jolta.specFor("", 21))
        assertEquals("21", Jolta.specFor("   ", 21))
    }

    @Test
    fun `the spec pins the major not the point release`() {
        // A pin of 21 keeps matching after `jolta upgrade`; 21.0.11 goes stale
        // the moment a patch lands. Exactness is opt-in, via --resolved.
        assertEquals("temurin@21", Jolta.specFor("temurin", Jolta.majorOf("21.0.11")!!))
    }

    /* ---------- B. validation before writing (jenv) ---------- */

    @Test
    fun `plain and vendor-qualified versions are pinnable`() {
        listOf("21", "25", "1.8", "21.0.4", "temurin@21", "corretto@11", "temurin-21", "graalvm@25")
            .forEach { assertTrue("should be pinnable: $it", Jolta.isPinnable(it)) }
    }

    @Test
    fun `the keyword specs jolta documents are pinnable`() {
        assertTrue(Jolta.isPinnable("lts"))
        assertTrue(Jolta.isPinnable("LTS"))
        assertTrue(Jolta.isPinnable("latest"))
    }

    @Test
    fun `a version jolta cannot parse is rejected before it reaches the file`() {
        // Verified against the CLI: each of these exits non-zero with
        // "cannot parse version" and writes nothing.
        listOf("", "   ", "java", "temurin", "temurin@", "-", "next", "v21")
            .forEach { assertFalse("should be rejected: '$it'", Jolta.isPinnable(it)) }
    }

    @Test
    fun `validation does not second-guess specs the CLI accepts`() {
        // "21.x" looks wrong but the CLI writes it happily and resolves it as
        // major 21, so refusing it in the dialog would be the plugin inventing
        // a rule the tool doesn't have.
        assertTrue(Jolta.isPinnable("21.x"))
        assertTrue(Jolta.isPinnable("21-ea"))
    }

    /* ---------- B2. punctuation is noise, not an error ---------- */

    @Test
    fun `a dangling separator is trimmed rather than rejected`() {
        // "@21" carries nothing a bare "21" doesn't. The CLI refuses it; making
        // the user fix their own typo would be pedantry, so normalize instead.
        assertEquals("21", Jolta.normalizeSpec("@21"))
        assertEquals("21", Jolta.normalizeSpec("-21"))
        assertEquals("21", Jolta.normalizeSpec("21@"))
        assertEquals("21", Jolta.normalizeSpec("  @21  "))
        listOf("@21", "-21", "21@", " @21 ").forEach {
            assertTrue("should be accepted after normalizing: '$it'", Jolta.isPinnable(it))
        }
    }

    @Test
    fun `normalizing never damages a spec that was already fine`() {
        listOf("21", "temurin@21", "temurin-21", "21.0.4", "21-ea", "1.8", "lts")
            .forEach { assertEquals(it, Jolta.normalizeSpec(it)) }
    }

    @Test
    fun `an unknown distribution is still refused rather than silently dropped`() {
        // Trimming "@" is safe because it means nothing. Discarding "adoptium"
        // would not be: it would pin a JDK the user didn't ask for.
        assertFalse(Jolta.isPinnable("adoptium@21"))
        assertFalse(Jolta.isPinnable("notavendor@21"))
        assertEquals("adoptium@21", Jolta.normalizeSpec("adoptium@21"))
    }

    @Test
    fun `a vendor with no version stays rejected`() {
        // Nothing to guess here — which temurin?
        assertEquals("temurin", Jolta.normalizeSpec("temurin@"))
        assertFalse(Jolta.isPinnable("temurin@"))
    }

    @Test
    fun `surrounding whitespace does not make a valid spec invalid`() {
        assertTrue(Jolta.isPinnable("  21  "))
        assertTrue(Jolta.isPinnable("\ttemurin@21\n"))
    }

    /* ---------- C. reporting failures (pathway for errors) ---------- */

    @Test
    fun `the last stderr line is what gets surfaced`() {
        val e = Jolta.Exec(
            rc = 1,
            stdout = "",
            stderr = "jolta: fetching temurin 21\njolta: error: no build of 'temurin@99' exists\n",
        )
        assertFalse(e.ok)
        assertEquals("jolta: error: no build of 'temurin@99' exists", e.message())
    }

    @Test
    fun `stdout is used when a failure said nothing on stderr`() {
        val e = Jolta.Exec(rc = 2, stdout = "could not write .java-version\n", stderr = "")
        assertEquals("could not write .java-version", e.message())
    }

    @Test
    fun `a silent failure still produces something to show`() {
        // An empty error dialog is worse than a vague one.
        val e = Jolta.Exec(rc = 3, stdout = "", stderr = "")
        assertTrue(e.message().isNotBlank())
        assertTrue(e.message().contains("3"))
    }

    @Test
    fun `a failure with no exit status at all is still reportable`() {
        val e = Jolta.Exec(rc = null, stdout = "", stderr = "")
        assertFalse(e.ok)
        assertTrue(e.message().isNotBlank())
    }

    @Test
    fun `blank lines do not become the error message`() {
        val e = Jolta.Exec(rc = 1, stdout = "", stderr = "jolta: error: disk full\n\n   \n")
        assertEquals("jolta: error: disk full", e.message())
    }

    /* ---------- D. round trip ---------- */

    @Test
    fun `every spec built from a listed JDK is one jolta would accept`() {
        // The picker and the status bar both build specs this way; a JDK that
        // appears in the list must always be pinnable from it.
        val listed = Jolta.parseJdks(
            """
            21	21.0.11	temurin	/h/temurin-21.0.11
            11	11.0.32	corretto	/h/corretto-11.0.32
            8	1.8.0_452	zulu	/h/zulu-8
            17	17.0.19	?	/h/mystery-17
            """.trimIndent(),
        )
        assertEquals(4, listed.size)
        listed.forEach { jdk ->
            val spec = Jolta.specFor(jdk.vendor, jdk.major)
            assertTrue("built an unpinnable spec '$spec' from $jdk", Jolta.isPinnable(spec))
        }
    }
}
