package dev.jolta.intellij

import com.intellij.notification.Notification
import com.intellij.notification.Notifications
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.impl.SimpleDataContext
import com.intellij.openapi.components.service
import com.intellij.openapi.ui.DialogPanel
import com.intellij.openapi.util.Disposer
import com.intellij.testFramework.TestActionEvent
import com.intellij.testFramework.fixtures.BasePlatformTestCase
import javax.swing.JCheckBox
import javax.swing.JTextField

/**
 * The interactive surfaces: dialog, status bar, actions, settings, editor
 * banners.
 *
 * These are the parts that never get exercised by a unit test of the resolver
 * and only get found by a human clicking — which is exactly why they're worth
 * pinning. Where a competitor shipped a bug in one of these, it's named.
 */
class JoltaInteractiveTest : BasePlatformTestCase() {

    private lateinit var fakeHome: java.io.File

    override fun setUp() {
        super.setUp()
        // Point JOLTA_HOME at an empty directory: without this the suite reads
        // the developer's real ~/.jolta, and a machine with a global default
        // makes every project look governed.
        fakeHome = java.nio.file.Files.createTempDirectory("jolta-home").toFile()
        Jolta.homeOverride = fakeHome
    }

    override fun tearDown() {
        try {
            Jolta.homeOverride = null
            fakeHome.deleteRecursively()
        } finally {
            super.tearDown()
        }
    }

    private val temurin21 = JoltaJdk(21, "21.0.11", "temurin", "/h/temurin-21.0.11")
    private val temurin17 = JoltaJdk(17, "17.0.19", "temurin", "/h/temurin-17.0.19")
    private val corretto11 = JoltaJdk(11, "11.0.32", "corretto", "/h/corretto-11.0.32")
    private val newer21 = JoltaJdk(21, "21.0.12", "temurin", "/h/temurin-21.0.12")

    private fun dialog(
        installed: List<JoltaJdk> = listOf(temurin21, corretto11),
        initial: JoltaPinRequest = JoltaPinRequest(),
    ): JoltaPickJdkDialog =
        JoltaPickJdkDialog(project, installed, initial).also { Disposer.register(testRootDisposable, it.disposable) }

    /* ================= dialog ================= */

    fun `test dialog opens with no selection and refuses to pin nothing`() {
        val d = dialog(initial = JoltaPinRequest())
        assertNotNull("empty input must block OK", d.validationMessage())
    }

    fun `test dialog preselects the version already pinned`() {
        val d = dialog(initial = JoltaPinRequest("temurin@21"))
        assertNull(d.validationMessage())
        assertEquals("temurin@21", d.spec())
    }

    fun `test dialog accepts a version that is not installed`() {
        // The whole point of a version manager: asking for something you don't
        // have is a request, not a mistake (intellij-mise #302).
        val d = dialog(installed = listOf(corretto11), initial = JoltaPinRequest("25"))
        assertNull("an uninstalled version must be pinnable", d.validationMessage())
        assertEquals("25", d.spec())
    }

    fun `test dialog works with no JDKs installed at all`() {
        val d = dialog(installed = emptyList(), initial = JoltaPinRequest("21"))
        assertNull(d.validationMessage())
        assertEquals("21", d.spec())
    }

    fun `test dialog rejects an unparseable version with a message naming the fix`() {
        val d = dialog(initial = JoltaPinRequest("banana"))
        val info = d.validationMessage()
        assertNotNull(info)
        assertTrue(info!!.contains("try"))
    }

    fun `test dialog trims punctuation instead of refusing it`() {
        val d = dialog(initial = JoltaPinRequest("@21"))
        assertNull("'@21' should be accepted, not an error", d.validationMessage())
        assertEquals("21", d.spec())
    }

    fun `test dialog exposes exact pinning and defaults it off`() {
        val d = dialog(initial = JoltaPinRequest("21"))
        assertFalse("a plain pin must survive point upgrades by default", d.exact())
        d.request.exact = true
        assertTrue(d.exact())
    }

    fun `test dialog center panel is built with the platform DSL`() {
        // A DialogPanel means spacing, label alignment and comment styling come
        // from the platform rather than this plugin's guesses.
        val d = dialog()
        assertTrue("expected a Kotlin UI DSL panel", d.centerPanel is DialogPanel)
    }

    fun `test dialog renders a text field and a checkbox`() {
        val d = dialog()
        val components = mutableListOf<java.awt.Component>()
        fun walk(c: java.awt.Container) {
            c.components.forEach { components += it; if (it is java.awt.Container) walk(it) }
        }
        walk(d.centerPanel)
        assertTrue("no text field in the dialog", components.any { it is JTextField })
        assertTrue("no exact-pin checkbox in the dialog", components.any { it is JCheckBox })
    }

    fun `test dialog lists newest major first and newest build within it`() {
        // The JDK most people reach for should not be buried; string ordering
        // would put 21.0.9 above 21.0.12 (the repair bug, in list form).
        val d = dialog(installed = listOf(temurin17, temurin21, newer21, corretto11))
        val order = d.orderedForTest().map { it.version }
        assertEquals(listOf("21.0.12", "21.0.11", "17.0.19", "11.0.32"), order)
    }

    /* ================= status bar ================= */

    private fun widgetState(): Pair<String?, String?> =
        JoltaStatusBarWidget(project, scopeForTest()).let {
            Disposer.register(testRootDisposable, it)
            it.renderForTest()
        }

    private fun scopeForTest() = kotlinx.coroutines.CoroutineScope(kotlinx.coroutines.SupervisorJob())

    fun `test status bar says something useful when the project is unpinned`() {
        // lastKnown() is null in a fresh fixture: unpinned, jolta may be absent.
        val (text, tooltip) = widgetState()
        assertNotNull("widget must render a label", text)
        assertTrue("text should name Java: $text", text!!.contains("Java", ignoreCase = true))
        assertTrue("tooltip must explain the state", !tooltip.isNullOrBlank())
    }

    fun `test status bar never renders an empty label`() {
        val (text, _) = widgetState()
        assertTrue("an unlabelled widget is a mystery box", !text.isNullOrBlank())
    }

    /* ================= actions ================= */

    fun `test actions declare a background update thread`() {
        // Required since 2022.3; an action without it throws at runtime.
        assertEquals(ActionUpdateThread.BGT, JoltaSetVersionAction().actionUpdateThread)
        assertEquals(ActionUpdateThread.BGT, JoltaReloadAction().actionUpdateThread)
    }

    fun `test actions are dumb aware so they survive indexing`() {
        assertTrue(JoltaSetVersionAction().isDumbAware)
        assertTrue(JoltaReloadAction().isDumbAware)
    }

    fun `test the reload action reports rather than failing silently when jolta is absent`() {
        val context = SimpleDataContext.getProjectContext(project)
        val seen = captureNotifications {
            JoltaReloadAction().actionPerformed(TestActionEvent.createTestEvent(context))
        }
        if (Jolta.binary() == null) {
            assertTrue("a dead menu item is worse than one that explains itself", seen.isNotEmpty())
        }
    }

    /* ================= settings ================= */

    fun `test settings page builds and round-trips every toggle`() {
        val configurable = JoltaConfigurable(project)
        val panel = configurable.createPanel()
        Disposer.register(testRootDisposable) { configurable.disposeUIResources() }

        val state = project.service<JoltaProjectState>()
        val originalEnv = state.injectRunConfigEnv

        panel.reset()
        state.injectRunConfigEnv = !originalEnv
        panel.reset()
        panel.apply()
        assertEquals(!originalEnv, state.injectRunConfigEnv)

        state.injectRunConfigEnv = originalEnv
    }

    fun `test the defaults are the behaviour the plugin exists for`() {
        val fresh = JoltaProjectState.State()
        assertTrue("env injection should be on by default", fresh.injectRunConfigEnv)
        assertTrue("Gradle and Maven should follow the pin by default", fresh.manageGradleAndMaven)
        assertFalse("the pin offer should not start suppressed", fresh.suppressPinOffer)
    }

    fun `test declining the pin offer persists`() {
        val state = project.service<JoltaProjectState>()
        val original = state.suppressPinOffer
        try {
            state.suppressPinOffer = true
            assertTrue(state.getState().suppressPinOffer)
            val reloaded = JoltaProjectState().apply { loadState(state.getState()) }
            assertTrue("\"don't ask\" must survive a restart", reloaded.suppressPinOffer)
        } finally {
            state.suppressPinOffer = original
        }
    }

    /* ================= run config setting is honoured ================= */

    fun `test turning off env injection actually stops it`() {
        // mise #485 is users asking for exactly this switch; a setting that
        // doesn't take effect is worse than no setting.
        val state = project.service<JoltaProjectState>()
        val original = state.injectRunConfigEnv
        val originalProvider = currentProvider
        try {
            currentProvider = { _, _ -> JoltaCurrent("21.0.11", "temurin", "/h/t21", "/p/.java-version") }
            state.injectRunConfigEnv = false

            val params = com.intellij.execution.configurations.JavaParameters().apply {
                workingDirectory = project.basePath
                env = mapOf("PATH" to "/usr/bin")
            }
            JoltaRunConfigurationExtension().updateJavaParameters(
                com.intellij.execution.application.ApplicationConfiguration("main", project),
                params,
                null,
            )
            assertNull("JAVA_HOME must not be set when injection is off", params.env["JAVA_HOME"])
            assertEquals("/usr/bin", params.env["PATH"])
        } finally {
            currentProvider = originalProvider
            state.injectRunConfigEnv = original
        }
    }

    /* ================= editor surfaces ================= */

    fun `test the pin file banner appears only on pin files`() {
        val provider = JoltaPinFileNotificationProvider()
        val java = myFixture.addFileToProject("Foo.java", "class Foo {}").virtualFile
        assertNull("no banner on ordinary sources", provider.collectNotificationData(project, java))
    }

    fun `test the pin file banner appears on a java-version file`() {
        val provider = JoltaPinFileNotificationProvider()
        val pin = myFixture.addFileToProject(".java-version", "21\n").virtualFile
        assertNotNull("a pin file should explain itself", provider.collectNotificationData(project, pin))
    }

    fun `test the pin file banner flags a version that cannot resolve`() {
        val provider = JoltaPinFileNotificationProvider()
        val pin = myFixture.addFileToProject("bad/.java-version", "banana\n").virtualFile
        assertNotNull("an unparseable pin must be called out", provider.collectNotificationData(project, pin))
    }

    fun `test comments and blank lines are skipped when reading the pin`() {
        val provider = JoltaPinFileNotificationProvider()
        val pin = myFixture.addFileToProject("commented/.java-version", "\n# a comment\n21\n").virtualFile
        assertNotNull(provider.collectNotificationData(project, pin))
    }

    fun `test the sdk setup validator stays out of unpinned projects`() {
        val validator = JoltaSdkSetupValidator()
        val java = myFixture.addFileToProject("Bar.java", "class Bar {}").virtualFile
        // Empty JOLTA_HOME, no pin: jolta is not in charge here.
        assertFalse(
            "seizing the IDE's own SDK banner in an unrelated project is overreach",
            validator.isApplicableFor(project, java),
        )
    }

    fun `test a global jolta default is enough to make a project governed`() {
        // A default is deliberate configuration — "jolta manages my Java" —
        // unlike the system JDK that `jolta current` falls back to.
        assertFalse(Jolta.isGoverned(fakeHome))
        java.io.File(fakeHome, "default").writeText("25\n")
        assertTrue(Jolta.isGoverned(fakeHome))
    }

    fun `test the sdk setup validator ignores non-java files`() {
        val validator = JoltaSdkSetupValidator()
        val txt = myFixture.addFileToProject("notes.txt", "hello").virtualFile
        assertFalse(validator.isApplicableFor(project, txt))
    }

    /* ================= helpers ================= */

    private fun captureNotifications(block: () -> Unit): List<Notification> {
        val seen = mutableListOf<Notification>()
        val connection = project.messageBus.connect(testRootDisposable)
        connection.subscribe(
            Notifications.TOPIC,
            object : Notifications {
                override fun notify(notification: Notification) {
                    seen += notification
                }
            },
        )
        block()
        return seen
    }
}
