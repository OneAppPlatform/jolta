package dev.jolta.intellij

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Per-module SDKs: "the most specific pin wins" applied at the module boundary.
 *
 * The CLI already resolves per directory — edge.sh section B pins that. This is
 * the IDE half, and it's the thing the competition doesn't do: intellij-mise
 * resolves once from the project root (`guessMiseProjectPath()`) and sets only
 * `projectSdk`, so every module in a monorepo compiles against the root's
 * version regardless of what its own config says. Their #26 is the same problem
 * reported from the other side.
 *
 * The half that's easy to get wrong is not assignment but **withdrawal**: a
 * module that had its own pin and lost it has to go back to inheriting, or it
 * keeps an SDK nobody asked for and editing `.java-version` never clears it.
 */
class JoltaModulePlanTest {

    private val root = JoltaCurrent("21.0.11", "temurin", "/h/t21", "/repo/.java-version")
    private val j17 = JoltaCurrent("17.0.19", "temurin", "/h/t17", "/repo/svc-a/.java-version")
    private val j11 = JoltaCurrent("11.0.32", "corretto", "/h/c11", "/repo/svc-b/.java-version")
    private val root2 = JoltaCurrent("25.0.3", "temurin", "/h/t25", "/repo/.java-version")

    private fun plan(
        projectPin: JoltaCurrent = root,
        modules: Map<String, JoltaCurrent> = emptyMap(),
        applied: Set<String> = emptySet(),
        known: Set<String> = modules.keys,
    ) = JoltaModulePlan.compute(projectPin, modules, applied, known)

    /* ---------- assignment ---------- */

    @Test
    fun `a module with its own pin gets its own SDK`() {
        val p = plan(modules = mapOf("svc-a" to j17))
        assertEquals(mapOf("svc-a" to j17), p.assign)
        assertTrue(p.revert.isEmpty())
    }

    @Test
    fun `siblings with different pins each get their own`() {
        val p = plan(modules = mapOf("svc-a" to j17, "svc-b" to j11))
        assertEquals(setOf("svc-a", "svc-b"), p.assign.keys)
        assertEquals(j17, p.assign["svc-a"])
        assertEquals(j11, p.assign["svc-b"])
    }

    @Test
    fun `a module matching the project inherits rather than duplicating`() {
        // A redundant module SDK is one more entry to go stale after upgrade.
        val p = plan(modules = mapOf("app" to root))
        assertTrue("should not assign an SDK identical to the project's", p.assign.isEmpty())
    }

    @Test
    fun `an unpinned module resolves to the project pin and inherits`() {
        // Walk-up means an unpinned module resolves to the root's pin, which is
        // the same object — so it must land in the inherit case, not assign.
        val p = plan(modules = mapOf("svc-a" to j17, "docs" to root))
        assertEquals(setOf("svc-a"), p.assign.keys)
    }

    @Test
    fun `a project with no modules plans nothing`() {
        assertTrue(plan().isEmpty)
    }

    /* ---------- withdrawal: the case that's easy to forget ---------- */

    @Test
    fun `a module that loses its pin is handed back to the project SDK`() {
        // svc-a had a pin, the file was deleted, so it now resolves to the root.
        val p = plan(modules = mapOf("svc-a" to root), applied = setOf("svc-a"))
        assertTrue(p.assign.isEmpty())
        assertEquals(setOf("svc-a"), p.revert)
    }

    @Test
    fun `a module stops needing its own SDK when the project pin moves to match`() {
        // Nobody touched svc-a; the project moved to 17, so svc-a's own 17 pin
        // is now redundant and it should inherit again.
        val p = plan(projectPin = j17, modules = mapOf("svc-a" to j17), applied = setOf("svc-a"))
        assertTrue(p.assign.isEmpty())
        assertEquals(setOf("svc-a"), p.revert)
    }

    @Test
    fun `a module deleted from the project is not reverted`() {
        // Nothing to hand back to — the module is gone. Attempting it would
        // just log a failure every sync forever.
        val p = plan(modules = emptyMap(), applied = setOf("gone"), known = emptySet())
        assertTrue(p.revert.isEmpty())
    }

    @Test
    fun `withdrawal and assignment happen in the same plan`() {
        // svc-a lost its pin, svc-b gained one, in one edit.
        val p = plan(
            modules = mapOf("svc-a" to root, "svc-b" to j11),
            applied = setOf("svc-a"),
        )
        assertEquals(mapOf("svc-b" to j11), p.assign)
        assertEquals(setOf("svc-a"), p.revert)
    }

    @Test
    fun `a still-differing module is reassigned, not reverted`() {
        val p = plan(modules = mapOf("svc-a" to j17), applied = setOf("svc-a"))
        assertEquals(mapOf("svc-a" to j17), p.assign)
        assertTrue("must not revert a module that still differs", p.revert.isEmpty())
    }

    /* ---------- changes over time ---------- */

    @Test
    fun `a module changing which JDK it pins is reassigned to the new one`() {
        val p = plan(modules = mapOf("svc-a" to j11), applied = setOf("svc-a"))
        assertEquals(j11, p.assign["svc-a"])
        assertTrue(p.revert.isEmpty())
    }

    @Test
    fun `moving the project pin leaves differing modules alone`() {
        // Project 21 -> 25. svc-a is pinned 17 and stays pinned 17.
        val p = plan(projectPin = root2, modules = mapOf("svc-a" to j17), applied = setOf("svc-a"))
        assertEquals(j17, p.assign["svc-a"])
        assertTrue(p.revert.isEmpty())
    }

    @Test
    fun `a module added later is picked up without disturbing the others`() {
        val p = plan(
            modules = mapOf("svc-a" to j17, "svc-new" to j11),
            applied = setOf("svc-a"),
        )
        assertEquals(setOf("svc-a", "svc-new"), p.assign.keys)
        assertTrue(p.revert.isEmpty())
    }

    @Test
    fun `a point-release upgrade counts as a different pin`() {
        // jolta upgrade moves the resolved home, so the module is reassigned
        // rather than being left on a directory that no longer exists.
        val upgraded = j17.copy(version = "17.0.20", home = "/h/t17.0.20")
        val p = plan(modules = mapOf("svc-a" to upgraded), applied = setOf("svc-a"))
        assertEquals("/h/t17.0.20", p.assign["svc-a"]?.home)
    }

    /* ---------- the plan is stable ---------- */

    @Test
    fun `computing twice from the same inputs gives the same plan`() {
        val a = plan(modules = mapOf("svc-a" to j17, "svc-b" to j11), applied = setOf("svc-a"))
        val b = plan(modules = mapOf("svc-a" to j17, "svc-b" to j11), applied = setOf("svc-a"))
        assertEquals(a, b)
    }

    @Test
    fun `a settled project plans no changes beyond re-asserting what is set`() {
        // Everything already applied and still differing: assign repeats, and
        // the executor skips modules whose SDK already matches.
        val p = plan(modules = mapOf("svc-a" to j17), applied = setOf("svc-a"))
        assertEquals(setOf("svc-a"), p.assign.keys)
        assertTrue(p.revert.isEmpty())
    }

    @Test
    fun `vendor differences alone are enough to warrant a module SDK`() {
        // Same major, different distro — corretto 21 is not temurin 21.
        val corretto21 = JoltaCurrent("21.0.11", "corretto", "/h/c21", "/repo/svc-a/.java-version")
        val p = plan(modules = mapOf("svc-a" to corretto21))
        assertEquals(setOf("svc-a"), p.assign.keys)
    }
}
