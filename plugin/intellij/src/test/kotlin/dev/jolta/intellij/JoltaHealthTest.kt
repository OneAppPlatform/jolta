package dev.jolta.intellij

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.projectRoots.JavaSdk
import com.intellij.openapi.projectRoots.ProjectJdkTable
import com.intellij.openapi.projectRoots.Sdk
import com.intellij.openapi.roots.ProjectRootManager
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import java.io.File
import java.nio.file.Files

/**
 * What counts as "out of sync".
 *
 * The governing rule: report what is **broken**, never what merely **differs**.
 * A Gradle toolchain compiling with 17 while the pin says 21 is correct — the
 * pin launches Gradle, the toolchain compiles. `org.gradle.java.home` is an
 * explicit override. A hand-picked Project SDK is a decision. Warning on any of
 * those is how mise ended up walking back #99 ("False positive warning SDK is
 * not configured correctly") and #354.
 *
 * So every case here is either a reference to a JDK that isn't on disk, or two
 * files giving contradictory instructions.
 */
class JoltaHealthTest : BasePlatformTestCase() {

    private lateinit var sandbox: File
    private val created = mutableListOf<Sdk>()

    override fun setUp() {
        super.setUp()
        sandbox = Files.createTempDirectory("jolta-health").toFile()
    }

    override fun tearDown() {
        try {
            ApplicationManager.getApplication().runWriteAction {
                val table = ProjectJdkTable.getInstance()
                ProjectRootManager.getInstance(project).projectSdk = null
                created.forEach { sdk -> table.findJdk(sdk.name)?.let(table::removeJdk) }
            }
            created.clear()
            sandbox.deleteRecursively()
        } finally {
            super.tearDown()
        }
    }

    private fun fakeJdkHome(name: String): File = File(sandbox, name).apply {
        File(this, "bin").mkdirs()
        File(this, "bin/java").apply { writeText("#!/bin/sh\n"); setExecutable(true) }
    }

    private fun installSdk(name: String, home: File): Sdk {
        val sdk = JavaSdk.getInstance().createJdk(name, home.absolutePath, false)
        ApplicationManager.getApplication().runWriteAction {
            ProjectJdkTable.getInstance().addJdk(sdk)
            ProjectRootManager.getInstance(project).projectSdk = sdk
        }
        created += sdk
        return sdk
    }

    private fun problems(dir: File = sandbox) = JoltaHealth.check(project, dir)

    /* ---------- silence on healthy and on merely-different ---------- */

    fun `test a healthy project reports nothing`() {
        installSdk("jolta temurin 21", fakeJdkHome("temurin-21.0.11"))
        assertEquals(emptyList<JoltaProblem>(), problems())
    }

    fun `test a project with no SDK at all is not a problem to report here`() {
        // Missing SDK is the SDK-setup validator's banner, not a balloon.
        assertTrue(problems().none { it.id.startsWith("dangling-sdk") })
    }

    fun `test an SDK that simply differs from the pin is not reported`() {
        // This is the false positive that must never fire: the user picked a
        // different JDK, or a toolchain governs compilation. Both are correct.
        installSdk("corretto-17", fakeJdkHome("corretto-17.0.19"))
        assertEquals(emptyList<JoltaProblem>(), problems())
    }

    /* ---------- the genuinely broken cases ---------- */

    fun `test an SDK whose directory vanished is reported`() {
        val home = fakeJdkHome("temurin-21.0.11")
        installSdk("jolta temurin 21", home)
        home.deleteRecursively()

        val found = problems()
        assertEquals(1, found.size)
        assertTrue(found[0].id.startsWith("dangling-sdk"))
        assertTrue("should name the SDK: ${found[0].message}", found[0].message.contains("jolta temurin 21"))
    }

    fun `test the dangling message says why it matters`() {
        val home = fakeJdkHome("temurin-21.0.11")
        installSdk("jolta temurin 21", home)
        home.deleteRecursively()
        assertTrue(problems()[0].message.contains("compile"))
    }

    /* ---------- contradictory pin files ---------- */

    fun `test two pin files that disagree are reported`() {
        File(sandbox, ".java-version").writeText("21\n")
        File(sandbox, ".sdkmanrc").writeText("java=17.0.9-tem\n")

        val conflict = problems().singleOrNull { it.id.startsWith("pin-conflict") }
        assertNotNull("a silently-ignored .sdkmanrc is worth saying out loud", conflict)
        assertTrue(conflict!!.message.contains(".java-version"))
        assertTrue(conflict.message.contains(".sdkmanrc"))
    }

    fun `test two pin files that agree are not reported`() {
        File(sandbox, ".java-version").writeText("21\n")
        File(sandbox, ".sdkmanrc").writeText("java=21.0.11-tem\n")
        assertTrue(problems().none { it.id.startsWith("pin-conflict") })
    }

    fun `test a sdkmanrc without a java line is not a conflict`() {
        File(sandbox, ".java-version").writeText("21\n")
        File(sandbox, ".sdkmanrc").writeText("gradle=8.5\nmaven=3.9.6\n")
        assertTrue(problems().none { it.id.startsWith("pin-conflict") })
    }

    fun `test comments in the pin file are skipped`() {
        File(sandbox, ".java-version").writeText("# team standard\n\n21\n")
        File(sandbox, ".sdkmanrc").writeText("java=21.0.11-tem\n")
        assertTrue(problems().none { it.id.startsWith("pin-conflict") })
    }

    fun `test a lone java-version is not a conflict`() {
        File(sandbox, ".java-version").writeText("21\n")
        assertTrue(problems().none { it.id.startsWith("pin-conflict") })
    }

    /* ---------- problem identity, so reporting can dedupe ---------- */

    fun `test the same problem keeps a stable id across checks`() {
        val home = fakeJdkHome("temurin-21.0.11")
        installSdk("jolta temurin 21", home)
        home.deleteRecursively()
        assertEquals(problems().map { it.id }, problems().map { it.id })
    }

    fun `test a resolved problem stops being reported`() {
        val home = fakeJdkHome("temurin-21.0.11")
        installSdk("jolta temurin 21", home)
        home.deleteRecursively()
        assertTrue(problems().isNotEmpty())

        home.mkdirs()
        assertTrue("a fixed problem must clear", problems().none { it.id.startsWith("dangling-sdk") })
    }
}
