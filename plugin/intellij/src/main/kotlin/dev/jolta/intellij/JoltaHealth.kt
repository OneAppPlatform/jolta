package dev.jolta.intellij

import com.intellij.openapi.project.Project
import com.intellij.openapi.projectRoots.ProjectJdkTable
import com.intellij.openapi.roots.ProjectRootManager
import java.io.File

/** Something actually broken, with the words to say about it. */
data class JoltaProblem(val id: String, val message: String)

/**
 * Checks for states that are *wrong*, not merely different.
 *
 * The distinction is the whole design. An IDE whose Java configuration differs
 * from the pin is usually correct: a Gradle toolchain
 * (`java.toolchain.languageVersion = 17`) deliberately compiles with a JDK other
 * than the one that launched Gradle, `org.gradle.java.home` is an explicit
 * override, and a hand-picked Project SDK is a choice someone made. Warning on
 * any of those fires constantly on correct projects — mise shipped that twice
 * (#99 "False positive warning SDK is not configured correctly", #354) and had
 * to walk it back both times.
 *
 * So nothing here reports a mismatch. Everything here reports a reference to a
 * JDK that isn't there, or two files giving contradictory instructions.
 */
object JoltaHealth {

    fun check(project: Project, projectDir: File): List<JoltaProblem> {
        val problems = mutableListOf<JoltaProblem>()
        problems += danglingProjectSdk(project)
        problems += danglingBuildToolJdks(project)
        problems += contradictoryPinFiles(projectDir)
        return problems
    }

    /**
     * The SDK exists in the table but its directory doesn't. This is what
     * IDEA-354569 leaves behind, and it turns the whole editor red. Reported
     * only after the repair pass has had its chance — if repair worked, there
     * is nothing to say.
     */
    private fun danglingProjectSdk(project: Project): List<JoltaProblem> {
        val sdk = ProjectRootManager.getInstance(project).projectSdk ?: return emptyList()
        val home = sdk.homePath ?: return emptyList()
        if (File(home).isDirectory) return emptyList()
        return listOf(
            JoltaProblem(
                "dangling-sdk:${sdk.name}",
                "The Project SDK '${sdk.name}' points at a directory that no longer exists ($home). " +
                    "Nothing will compile until it is repointed.",
            ),
        )
    }

    /**
     * Gradle or Maven naming an SDK the IDE doesn't have. Unlike a version
     * *mismatch*, this cannot be deliberate — the import will simply fail.
     */
    private fun danglingBuildToolJdks(project: Project): List<JoltaProblem> {
        val table = ProjectJdkTable.getInstance()
        fun missing(name: String?): Boolean {
            if (name.isNullOrBlank()) return false
            // Gradle accepts macros as well as SDK names; those are not ours to judge.
            if (name.startsWith("#")) return false
            val sdk = table.findJdk(name) ?: return true
            return sdk.homePath?.let { !File(it).isDirectory } ?: false
        }

        val problems = mutableListOf<JoltaProblem>()
        GradleApplier.currentJvms(project).forEach { jvm ->
            if (missing(jvm)) {
                problems += JoltaProblem(
                    "dangling-gradle:$jvm",
                    "Gradle is set to use the JDK '$jvm', which this IDE doesn't have. Gradle import will fail.",
                )
            }
        }
        val mavenJdk = MavenApplier.currentImporterJdk(project)
        if (missing(mavenJdk)) {
            problems += JoltaProblem(
                "dangling-maven:$mavenJdk",
                "The Maven importer is set to use the JDK '$mavenJdk', which this IDE doesn't have.",
            )
        }
        return problems
    }

    /**
     * Both pin files present and disagreeing. jolta silently prefers
     * `.java-version`, which is the right default and a genuinely confusing one
     * to hit — the `.sdkmanrc` you edited simply has no effect.
     */
    private fun contradictoryPinFiles(dir: File): List<JoltaProblem> {
        val javaVersion = firstSpec(File(dir, ".java-version")) ?: return emptyList()
        val sdkmanrc = sdkmanrcSpec(File(dir, ".sdkmanrc")) ?: return emptyList()
        if (Jolta.majorOf(javaVersion) == Jolta.majorOf(sdkmanrc)) return emptyList()
        return listOf(
            JoltaProblem(
                "pin-conflict:$javaVersion:$sdkmanrc",
                "This directory has both .java-version ($javaVersion) and .sdkmanrc ($sdkmanrc). " +
                    "jolta uses .java-version, so the .sdkmanrc value is being ignored.",
            ),
        )
    }

    private fun firstSpec(file: File): String? =
        try {
            file.takeIf { it.isFile }?.readLines()
                ?.map { it.trim() }
                ?.firstOrNull { it.isNotEmpty() && !it.startsWith("#") }
        } catch (e: Exception) {
            null
        }

    /** `.sdkmanrc` is key=value; only the java line concerns us. */
    private fun sdkmanrcSpec(file: File): String? =
        try {
            file.takeIf { it.isFile }?.readLines()
                ?.map { it.trim() }
                ?.firstOrNull { it.startsWith("java=") }
                ?.substringAfter("java=")
                ?.takeIf { it.isNotBlank() }
        } catch (e: Exception) {
            null
        }
}
