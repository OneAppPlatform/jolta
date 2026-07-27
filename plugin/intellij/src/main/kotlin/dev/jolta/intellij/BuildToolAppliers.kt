package dev.jolta.intellij

import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.project.Project
import org.jetbrains.idea.maven.project.MavenProjectsManager
import org.jetbrains.plugins.gradle.settings.GradleSettings

/**
 * The project SDK alone doesn't govern builds IDEA delegates to Gradle or
 * Maven — those read their own JVM settings. These appliers point both at
 * the jolta-managed SDK by name.
 *
 * Each is only touched inside a try/catch for NoClassDefFoundError: the
 * Gradle/Maven plugins are optional dependencies, and the JVM resolves
 * these classes lazily at first call, so the guard is sufficient when a
 * host IDE has them disabled.
 */
object GradleApplier {
    private val log = Logger.getInstance(GradleApplier::class.java)

    fun apply(project: Project, sdkName: String) {
        try {
            for (settings in GradleSettings.getInstance(project).linkedProjectsSettings) {
                if (settings.gradleJvm != sdkName) settings.gradleJvm = sdkName
            }
        } catch (e: NoClassDefFoundError) {
            // Gradle plugin not available in this IDE — nothing to do
        } catch (e: Exception) {
            log.warn("cannot set Gradle JVM", e)
        }
    }
}

object MavenApplier {
    private val log = Logger.getInstance(MavenApplier::class.java)

    fun apply(project: Project, sdkName: String) {
        try {
            val manager = MavenProjectsManager.getInstance(project)
            if (!manager.isMavenizedProject) return
            manager.importingSettings.jdkForImporter = sdkName
        } catch (e: NoClassDefFoundError) {
            // Maven plugin not available in this IDE — nothing to do
        } catch (e: Exception) {
            log.warn("cannot set Maven importer JDK", e)
        }
    }
}
