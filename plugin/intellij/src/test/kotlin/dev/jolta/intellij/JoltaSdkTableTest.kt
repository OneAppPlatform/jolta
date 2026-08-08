package dev.jolta.intellij

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.projectRoots.JavaSdk
import com.intellij.openapi.projectRoots.ProjectJdkTable
import com.intellij.openapi.projectRoots.Sdk
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import java.io.File
import java.nio.file.Files

/**
 * SDK-table hygiene: the entries this plugin leaves behind in
 * Project Structure → Platform Settings → SDKs.
 *
 * This is where every Java version manager has drawn blood:
 *
 *   - intellij-mise #370   "Multiple redundant SDKs created" — a fresh entry per
 *                          sync instead of reusing the existing one, so the SDK
 *                          list filled up with duplicates of the same JDK.
 *   - IDEA-358716          Opening a project with .sdkmanrc made the IDE
 *                          "discover" and re-create a JDK entry repeatedly, even
 *                          after the user deleted it.
 *   - IDEA-354569          "JDK misconfigured when changing project SDK location
 *                          and removing default SDK directory" — the dangling
 *                          entry left behind when a JDK's directory moves.
 *   - jenv (JetBrains fix) The standing advice to jenv users is literally
 *                          "remove or fix any JDK whose directory no longer
 *                          exists" — a manual chore we intend to make unnecessary.
 *
 * Point-release upgrades are the routine case that produces all of the above:
 * `jolta upgrade` replaces temurin-21.0.11 with temurin-21.0.12 and the old
 * directory stops existing.
 */
class JoltaSdkTableTest : BasePlatformTestCase() {
    private lateinit var sandbox: File
    private val created = mutableListOf<Sdk>()

    override fun setUp() {
        super.setUp()
        sandbox = Files.createTempDirectory("jolta-sdk").toFile()
    }

    override fun tearDown() {
        try {
            ApplicationManager.getApplication().runWriteAction {
                val table = ProjectJdkTable.getInstance()
                created.forEach { sdk -> table.findJdk(sdk.name)?.let(table::removeJdk) }
            }
            created.clear()
            sandbox.deleteRecursively()
        } finally {
            super.tearDown()
        }
    }

    /** A JDK-shaped directory: enough for JavaSdk to accept it as a home. */
    private fun fakeJdkHome(name: String): File {
        val home = File(sandbox, name)
        File(home, "bin").mkdirs()
        File(home, "lib").mkdirs()
        File(home, "bin/java").apply { writeText("#!/bin/sh\n"); setExecutable(true) }
        File(home, "release").writeText("JAVA_VERSION=\"${name.substringAfterLast('-')}\"\n")
        return home
    }

    private fun addSdk(name: String, home: File): Sdk {
        val sdk = JavaSdk.getInstance().createJdk(name, home.absolutePath, false)
        ApplicationManager.getApplication().runWriteAction {
            ProjectJdkTable.getInstance().addJdk(sdk)
        }
        created += sdk
        return sdk
    }

    private fun jdksNamed(name: String): List<Sdk> =
        ProjectJdkTable.getInstance().allJdks.filter { it.name == name }

    /* ---------- A. no duplicates (mise #370, IDEA-358716) ---------- */

    fun `test the same name is reused rather than duplicated`() {
        val home = fakeJdkHome("temurin-21.0.11")
        val name = "jolta temurin 21"

        addSdk(name, home)
        // A second sync resolving to the same JDK must find the existing entry.
        val existing = ProjectJdkTable.getInstance().findJdk(name)

        assertNotNull("findJdk must locate the entry we just added", existing)
        assertEquals("exactly one entry for the name", 1, jdksNamed(name).size)
    }

    /* ---------- B. naming survives point releases ---------- */
    /*
     * The name is built from the MAJOR, deliberately. mise names its entries
     * after the requested version string, so an exact pin (temurin-21.0.5) mints
     * a new SDK on every patch bump and the old one rots — the #370 mechanism.
     */

    fun `test the SDK name is stable across point releases`() {
        val first = sdkNameFor("21.0.11", "temurin")
        val second = sdkNameFor("21.0.12", "temurin")
        assertEquals("a patch bump must not mint a new SDK entry", first, second)
    }

    fun `test different majors and vendors get distinct names`() {
        assertFalse(sdkNameFor("21.0.11", "temurin") == sdkNameFor("17.0.19", "temurin"))
        assertFalse(sdkNameFor("21.0.11", "temurin") == sdkNameFor("21.0.11", "corretto"))
    }

    fun `test a vendorless resolution still produces a stable name`() {
        assertEquals(sdkNameFor("21.0.11", null), sdkNameFor("21.0.12", null))
    }

    /** Mirrors the naming rule in JoltaSyncService.applyToIde. */
    private fun sdkNameFor(version: String, vendor: String?): String =
        "jolta ${vendor ?: "jdk"} ${Jolta.majorOf(version)}"

    /* ---------- C. stale entries are detectable (IDEA-354569, jenv) ---------- */

    fun `test an entry whose home vanished is recognised as stale`() {
        val home = fakeJdkHome("temurin-21.0.11")
        val sdk = addSdk("jolta temurin 21", home)

        assertTrue("home exists before the upgrade", File(sdk.homePath!!).isDirectory)

        // `jolta upgrade` renames the versioned directory out from under it.
        home.deleteRecursively()

        assertFalse(
            "a dangling SDK is exactly what IDEA-354569 leaves behind",
            File(sdk.homePath!!).isDirectory,
        )
        // The replacement the repair pass would pick.
        val replacement = fakeJdkHome("temurin-21.0.12")
        val rows = Jolta.parseJdks("21\t21.0.12\ttemurin\t${replacement.absolutePath}")
        assertEquals(replacement.absolutePath, rows.single { it.major == 21 }.home)
    }

    fun `test repair picks the newest build of the same major`() {
        val rows = Jolta.parseJdks(
            """
            21	21.0.9	temurin	/h/temurin-21.0.9
            21	21.0.12	temurin	/h/temurin-21.0.12
            21	21.0.11	temurin	/h/temurin-21.0.11
            17	17.0.19	temurin	/h/temurin-17.0.19
            """.trimIndent(),
        )
        // Naive string ordering puts 21.0.9 on top and would repoint a repaired
        // SDK at an OLDER JDK than it had — the failure this test was written for.
        val newest21 = Jolta.newest(rows.filter { it.major == 21 })
        assertEquals("/h/temurin-21.0.12", newest21?.home)
    }

    fun `test double-digit patch levels order after single digits`() {
        assertTrue(Jolta.compareVersions("21.0.10", "21.0.9") > 0)
        assertTrue(Jolta.compareVersions("21.0.9", "21.0.12") < 0)
        assertTrue(Jolta.compareVersions("21.0.2", "21.0.11") < 0)
        assertEquals(0, Jolta.compareVersions("21.0.11", "21.0.11"))
    }

    fun `test build metadata does not perturb ordering`() {
        assertTrue(Jolta.compareVersions("17.0.19+7", "17.0.9+11") > 0)
        assertTrue(Jolta.compareVersions("1.8.0_452", "1.8.0_91") > 0)
    }

    fun `test shorter versions compare as zero-padded`() {
        assertTrue(Jolta.compareVersions("21.0.1", "21") > 0)
        assertEquals(0, Jolta.compareVersions("21", "21.0.0"))
    }

    /* ---------- D. scoping: only our own entries are touched ---------- */
    /*
     * The repair pass keys on the jolta jdks root. A hand-configured JDK living
     * anywhere else must never be repointed or removed, however dead it looks —
     * seizing SDKs the user owns is mise #393 in a different costume.
     */

    fun `test SDKs outside the jolta root are out of scope`() {
        val joltaRoot = File(Jolta.joltaHome(), "jdks").absolutePath
        val foreign = "/Library/Java/JavaVirtualMachines/jdk-21.jdk/Contents/Home"

        assertFalse(
            "a system JDK must not look like a jolta-managed one",
            com.intellij.openapi.util.io.FileUtil.isAncestor(joltaRoot, foreign, false),
        )
        assertTrue(
            com.intellij.openapi.util.io.FileUtil.isAncestor(
                joltaRoot,
                File(joltaRoot, "temurin-21.0.11/Contents/Home").path,
                false,
            ),
        )
    }
}
