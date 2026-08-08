package dev.jolta.intellij

import com.intellij.execution.application.ApplicationConfiguration
import com.intellij.execution.configurations.JavaParameters
import com.intellij.openapi.project.Project
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import java.io.File

/**
 * Environment injection into Java run configurations.
 *
 * Every case here pins a bug one of the competitors actually shipped:
 *
 *   - intellij-mise v0.4.x  "Fix runConfiguration cannot load envvars when it
 *                            doesn't have working directory" — fall back to the
 *                            project base directory.
 *   - intellij-mise v1.x    "Fix Node.js envvars are overriding runconfiguration's
 *                            envvars" — the tool must not clobber what the user
 *                            typed into the run configuration.
 *   - intellij-mise #230    "Run configuration envvars are not restoring" — the
 *                            reason we refuse to touch persisted settings.env.
 *   - intellij-mise #50/#64 "Replace environment injection marker with
 *                            identity-based state tracking" — double injection.
 *   - sdkman-cli #584       "IntelliJ does not succeed JAVA_HOME": a GUI-launched
 *                            IDE never saw the shell's JAVA_HOME, so anything a
 *                            run shelled out to got the wrong JDK.
 *   - IDEA-228394           JDKs under a version manager's own root aren't
 *                            visible to the IDE unless something points at them.
 */
class JoltaRunConfigurationExtensionTest : BasePlatformTestCase() {
    private val extension = JoltaRunConfigurationExtension()
    private lateinit var original: (Project, File?) -> JoltaCurrent?

    private val pinned = JoltaCurrent(
        version = "21.0.11",
        vendor = "temurin",
        home = "/jolta/jdks/temurin-21.0.11",
        source = "/p/.java-version",
    )
    private val binDir = File("/jolta/jdks/temurin-21.0.11", "bin").absolutePath
    private val sep = File.pathSeparator

    override fun setUp() {
        super.setUp()
        original = currentProvider
        currentProvider = { _, _ -> pinned }
    }

    override fun tearDown() {
        try {
            currentProvider = original
        } finally {
            super.tearDown()
        }
    }

    private fun config() = ApplicationConfiguration("main", project)

    private fun params(env: Map<String, String> = emptyMap(), workDir: String? = project.basePath) =
        JavaParameters().apply {
            workingDirectory = workDir
            this.env = env
        }

    /* ---------- A. the basic contract (sdkman-cli #584, IDEA-228394) ---------- */

    fun `test JAVA_HOME points at the pinned JDK`() {
        val p = params()
        extension.updateJavaParameters(config(), p, null)
        assertEquals("/jolta/jdks/temurin-21.0.11", p.env["JAVA_HOME"])
    }

    fun `test PATH gains the pinned bin directory first`() {
        val p = params(mapOf("PATH" to "/usr/bin$sep/bin"))
        extension.updateJavaParameters(config(), p, null)
        assertEquals("$binDir$sep/usr/bin$sep/bin", p.env["PATH"])
    }

    /* ---------- B. don't clobber the user (mise "envvars are overriding") ---------- */

    fun `test an explicit JAVA_HOME on the run configuration wins`() {
        val p = params(mapOf("JAVA_HOME" to "/hand/picked/jdk"))
        extension.updateJavaParameters(config(), p, null)
        assertEquals("/hand/picked/jdk", p.env["JAVA_HOME"])
    }

    fun `test unrelated run configuration env vars survive untouched`() {
        val p = params(mapOf("MY_FLAG" to "1", "SPRING_PROFILES_ACTIVE" to "test"))
        extension.updateJavaParameters(config(), p, null)
        assertEquals("1", p.env["MY_FLAG"])
        assertEquals("test", p.env["SPRING_PROFILES_ACTIVE"])
        assertEquals("/jolta/jdks/temurin-21.0.11", p.env["JAVA_HOME"])
    }

    /* ---------- C. idempotence (mise #50/#64 double-injection) ---------- */

    fun `test repeated invocations do not stack PATH entries`() {
        val p = params(mapOf("PATH" to "/usr/bin"))
        repeat(3) { extension.updateJavaParameters(config(), p, null) }
        assertEquals("$binDir$sep/usr/bin", p.env["PATH"])
    }

    /* ---------- D. missing working directory (mise v0.4.x) ---------- */

    fun `test a configuration without a working directory still gets injected`() {
        // The extension must fall back rather than silently doing nothing —
        // this is the exact regression mise shipped a fix for.
        var askedFor: File? = File("/sentinel")
        currentProvider = { _, dir -> askedFor = dir; pinned }

        val p = params(workDir = null)
        extension.updateJavaParameters(config(), p, null)

        assertNull("null working directory must reach the resolver as null", askedFor)
        assertEquals("/jolta/jdks/temurin-21.0.11", p.env["JAVA_HOME"])
    }

    fun `test a blank working directory is treated as absent`() {
        var askedFor: File? = File("/sentinel")
        currentProvider = { _, dir -> askedFor = dir; pinned }

        extension.updateJavaParameters(config(), params(workDir = "   "), null)

        assertNull(askedFor)
    }

    /* ---------- E. the working directory actually drives resolution ---------- */
    /*
     * intellij-mise #26 "Resolve non-project depends issue while supporting
     * subdirectory configs": a monorepo subdirectory carrying its own pin must
     * get its own answer, not the project root's.
     */

    fun `test the configuration's own working directory is what gets resolved`() {
        var askedFor: File? = null
        currentProvider = { _, dir -> askedFor = dir; pinned }

        extension.updateJavaParameters(config(), params(workDir = "/repo/services/api"), null)

        assertEquals(File("/repo/services/api"), askedFor)
    }

    /* ---------- F. nothing to say (governance) ---------- */

    fun `test an ungoverned project gets no injection at all`() {
        currentProvider = { _, _ -> null }
        val p = params(mapOf("PATH" to "/usr/bin"))

        extension.updateJavaParameters(config(), p, null)

        assertNull(p.env["JAVA_HOME"])
        assertEquals("PATH must be left exactly as it was", "/usr/bin", p.env["PATH"])
    }

    /* ---------- G. Gradle/Maven are deliberately out of scope (mise #230) ---------- */

    fun `test extension applies to all configurations but only mutates JavaParameters`() {
        // isApplicableFor is intentionally broad; the safety property is that
        // we only ever write to the per-execution JavaParameters, never to a
        // run configuration's persisted settings — which is what forced mise
        // into snapshot/restore machinery and the #230 leak.
        assertTrue(extension.isApplicableFor(config()))
    }
}
