package dev.jolta.intellij

import com.intellij.execution.RunConfigurationExtension
import com.intellij.execution.configurations.JavaParameters
import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.openapi.components.service
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.project.Project
import java.io.File

/**
 * The project SDK governs compilation, but a run configuration that shells out
 * — a test spawning a process, an exec task, a build script reading JAVA_HOME —
 * sees whatever environment the IDE itself was launched with. That's the same
 * ambient-JDK problem jolta exists to remove, one layer down.
 *
 * So every Java run configuration gets JAVA_HOME and a PATH entry pointing at
 * the pin in effect for its working directory.
 *
 * This deliberately does NOT touch ExternalSystemRunConfiguration (Gradle,
 * Maven). Their JDK already comes from the Gradle JVM and Maven importer
 * settings, which [GradleApplier]/[MavenApplier] set from the same pin, and the
 * only way to reach their environment is to mutate `settings.env` — persisted
 * state that then has to be snapshotted and restored around every execution.
 * Not worth the footgun for something already covered.
 */
class JoltaRunConfigurationExtension : RunConfigurationExtension() {
    private val log = Logger.getInstance(JoltaRunConfigurationExtension::class.java)

    override fun isApplicableFor(configuration: RunConfigurationBase<*>): Boolean = true

    override fun <T : RunConfigurationBase<*>> updateJavaParameters(
        configuration: T,
        params: JavaParameters,
        runnerSettings: RunnerSettings?,
    ) {
        val project = configuration.project
        if (project.isDisposed) return
        if (!project.service<JoltaProjectState>().injectRunConfigEnv) return

        val workDir = params.workingDirectory?.takeIf { it.isNotBlank() }?.let(::File)
        val current = currentProvider(project, workDir) ?: return

        val env = LinkedHashMap(params.env)

        // A JAVA_HOME set on the run configuration itself is more specific than
        // the project's pin — the user asked for it right here, so leave it be.
        val explicit = env.keys.firstOrNull { it.equals("JAVA_HOME", ignoreCase = true) }
        if (explicit != null) {
            log.debug("run config '${configuration.name}' sets $explicit explicitly — leaving it alone")
        } else {
            env["JAVA_HOME"] = current.home
        }

        val binDir = File(current.home, "bin").absolutePath
        val pathKey = env.keys.firstOrNull { it.equals("PATH", ignoreCase = true) }
            ?: if (System.getProperty("os.name", "").startsWith("Windows")) "Path" else "PATH"
        // Inherited parent env isn't in params.env, so fall back to ours when the
        // run config doesn't override PATH itself.
        val basePath = env[pathKey] ?: System.getenv("PATH")
        env[pathKey] = when {
            basePath.isNullOrBlank() -> binDir
            // already first: re-running the same config shouldn't stack entries
            basePath.startsWith("$binDir${File.pathSeparator}") || basePath == binDir -> basePath
            else -> "$binDir${File.pathSeparator}$basePath"
        }

        params.env = env
    }
}

/**
 * Seam for tests: the real resolution needs a running jolta CLI, which a
 * platform test fixture has no business spawning.
 */
internal var currentProvider: (Project, File?) -> JoltaCurrent? = { project, workDir ->
    project.service<JoltaSyncService>().currentFor(workDir)
}
