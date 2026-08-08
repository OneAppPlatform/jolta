package dev.jolta.intellij

import com.intellij.openapi.components.service
import com.intellij.testFramework.fixtures.BasePlatformTestCase

/**
 * Manual overrides versus the pin.
 *
 * jolta's stamp is global — `$JOLTA_HOME/.stamp`, bumped by `install`, `pin`,
 * `upgrade` and `uninstall` from *any* directory — so every open project
 * re-syncs whenever anything happens anywhere. Re-asserting unconditionally
 * meant a JDK someone chose in Project Structure reverted minutes later with no
 * explanation.
 *
 * The rule these tests pin: a manual change is respected until the pin itself
 * moves. A new pin is new information and wins; an unrelated `jolta install` is
 * not, and must not.
 */
class JoltaOverrideTest : BasePlatformTestCase() {

    private val pin21 = JoltaCurrent("21.0.11", "temurin", "/h/temurin-21.0.11", "/p/.java-version")
    private val pin21patch = JoltaCurrent("21.0.12", "temurin", "/h/temurin-21.0.12", "/p/.java-version")
    private val pin17 = JoltaCurrent("17.0.19", "temurin", "/h/temurin-17.0.19", "/p/.java-version")

    private fun key(c: JoltaCurrent) = JoltaOverride.pinKey(c)

    private fun state() = project.service<JoltaProjectState>()

    override fun tearDown() {
        try {
            state().appliedSdkName = null
            state().appliedPinKey = null
        } finally {
            super.tearDown()
        }
    }

    /** The real decision the sync service makes — not a copy of it. */
    private fun overridden(appliedSdk: String?, appliedPin: String?, actualSdk: String?, current: JoltaCurrent): Boolean =
        JoltaOverride.isManualOverride(appliedSdk, appliedPin, actualSdk, current)

    fun `test an untouched project is not an override`() {
        assertFalse(overridden("jolta temurin 21", key(pin21), "jolta temurin 21", pin21))
    }

    fun `test a project we have never applied to is not an override`() {
        // First sync of a fresh project must not be mistaken for a user choice.
        assertFalse(overridden(null, null, "some-existing-sdk", pin21))
    }

    fun `test a hand-picked SDK counts as an override`() {
        assertTrue(overridden("jolta temurin 21", key(pin21), "corretto-17", pin21))
    }

    fun `test an unrelated jolta command does not clear the override`() {
        // `jolta install 25` elsewhere bumps the global stamp and re-syncs this
        // project, but the pin is unchanged — the user's choice must survive.
        assertTrue(overridden("jolta temurin 21", key(pin21), "corretto-17", pin21))
    }

    fun `test changing the pin overrules a manual choice`() {
        // A new pin is new information: it wins.
        assertFalse(overridden("jolta temurin 21", key(pin21), "corretto-17", pin17))
    }

    fun `test a point-release upgrade also overrules the manual choice`() {
        // jolta upgrade moves the resolved home, so the pin key moves too.
        assertFalse(overridden("jolta temurin 21", key(pin21), "corretto-17", pin21patch))
    }

    fun `test clearing the project SDK entirely counts as an override`() {
        assertTrue(overridden("jolta temurin 21", key(pin21), null, pin21))
    }

    /* ---------- persistence ---------- */

    fun `test what we applied survives a restart`() {
        val s = state()
        s.recordApplied("jolta temurin 21", key(pin21))

        val reloaded = JoltaProjectState().apply { loadState(s.getState()) }
        assertEquals("jolta temurin 21", reloaded.appliedSdkName)
        assertEquals(key(pin21), reloaded.appliedPinKey)
    }

    fun `test a fresh project has recorded nothing`() {
        val fresh = JoltaProjectState.State()
        assertNull(fresh.appliedSdkName)
        assertNull(fresh.appliedPinKey)
    }

    fun `test the pin key distinguishes vendor version and home`() {
        assertFalse(key(pin21) == key(pin17))
        assertFalse(key(pin21) == key(pin21patch))
        assertFalse(
            "same version, different vendor must be a different pin",
            key(pin21) == key(pin21.copy(vendor = "corretto")),
        )
        assertEquals(key(pin21), key(pin21.copy()))
    }

    fun `test a vendorless pin still produces a stable key`() {
        val vendorless = pin21.copy(vendor = null)
        assertEquals(key(vendorless), key(vendorless.copy()))
        assertFalse(key(vendorless) == key(pin21))
    }
}
