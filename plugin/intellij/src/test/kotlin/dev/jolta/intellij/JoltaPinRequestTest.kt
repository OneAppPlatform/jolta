package dev.jolta.intellij

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The model behind the picker, tested without any Swing.
 *
 * Validation lives in [JoltaPinRequest] precisely so it can be hammered like
 * this — the dialog, the status bar popup, and the notification actions all
 * route through it, so a rule proved here holds at every entry point.
 */
class JoltaPinRequestTest {

    private fun err(spec: String): String? = JoltaPinRequest(spec).validate()

    /* ---------- what passes ---------- */

    @Test
    fun `specs the CLI accepts validate clean`() {
        listOf("21", "25", "1.8", "21.0.4", "temurin@21", "corretto@11", "temurin-21", "lts", "21.x", "21-ea")
            .forEach { assertNull("should validate: $it", err(it)) }
    }

    @Test
    fun `punctuation-only noise validates clean and is trimmed on the way out`() {
        listOf("@21", "-21", "21@", "  @21  ").forEach {
            assertNull("should validate: '$it'", err(it))
            assertEquals("21", JoltaPinRequest(it).normalized())
        }
    }

    /* ---------- what fails, and whether the message helps ---------- */

    @Test
    fun `an empty request asks for input rather than complaining`() {
        assertEquals("Choose a JDK or type a version", err(""))
        assertEquals("Choose a JDK or type a version", err("   "))
    }

    @Test
    fun `an unknown distribution is named in the message`() {
        val message = err("adoptium@21")
        assertNotNull(message)
        assertTrue("should name what it didn't recognise: $message", message!!.contains("adoptium"))
        assertTrue("should suggest real ones: $message", message.contains("temurin"))
    }

    @Test
    fun `an unparseable version gets the version message not the vendor one`() {
        val message = err("banana")
        assertNotNull(message)
        assertTrue(message!!.contains("Not a version jolta can pin"))
    }

    @Test
    fun `a vendor with no version is refused`() {
        assertNotNull(err("temurin@"))
    }

    @Test
    fun `every failure message is something a human can act on`() {
        listOf("", "banana", "adoptium@21", "temurin@", "v21").forEach { spec ->
            val message = err(spec)
            assertNotNull("no message for '$spec'", message)
            assertTrue("message too terse for '$spec': $message", message!!.length > 15)
            assertTrue(
                "message should suggest what to do for '$spec': $message",
                message.contains("try") || message.contains("Choose"),
            )
        }
    }

    /* ---------- construction from a listed JDK ---------- */

    @Test
    fun `a request built from a listed JDK always validates`() {
        val listed = Jolta.parseJdks(
            """
            21	21.0.11	temurin	/h/a
            11	11.0.32	corretto	/h/b
            8	1.8.0_452	zulu	/h/c
            17	17.0.19	?	/h/d
            """.trimIndent(),
        )
        listed.forEach { jdk ->
            val request = JoltaPinRequest.forJdk(jdk)
            assertNull("built an invalid request from $jdk: ${request.validate()}", request.validate())
        }
    }

    @Test
    fun `a JDK with an unknown vendor pins by major alone`() {
        val jdk = JoltaJdk(major = 17, version = "17.0.19", vendor = "?", home = "/h/d")
        assertEquals("17", JoltaPinRequest.forJdk(jdk).normalized())
    }

    @Test
    fun `exact defaults off so the ordinary pin survives upgrades`() {
        assertEquals(false, JoltaPinRequest("21").exact)
        assertEquals(false, JoltaPinRequest.forJdk(JoltaJdk(21, "21.0.11", "temurin", "/h")).exact)
    }

    @Test
    fun `the model is a value so editing a copy cannot disturb the original`() {
        val initial = JoltaPinRequest("temurin@21")
        val editing = initial.copy()
        editing.spec = "corretto@17"
        editing.exact = true
        assertEquals("temurin@21", initial.spec)
        assertEquals(false, initial.exact)
    }
}
