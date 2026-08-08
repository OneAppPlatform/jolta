package dev.jolta.intellij

import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.service
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.projectRoots.JavaSdk
import com.intellij.openapi.projectRoots.ProjectJdkTable
import com.intellij.openapi.projectRoots.Sdk
import com.intellij.openapi.roots.ProjectRootManager
import com.intellij.openapi.util.io.FileUtil
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import com.intellij.openapi.vfs.VirtualFileManager
import com.intellij.util.concurrency.AppExecutorUtil
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Keeps the IDE in lockstep with jolta for one project:
 *
 *  - resolves the project's pin via `jolta current --json`
 *  - registers/repairs the SDK ("jolta <vendor> <major>" — the MAJOR, so the
 *    entry survives point-release upgrades by repointing, never dangling)
 *  - sets the project SDK, the Gradle JVM, and the Maven importer JDK
 *  - re-syncs when .java-version/.sdkmanrc change in the project, or when
 *    jolta's stamp file changes (pin/install/upgrade from any terminal)
 *  - offers auto-install when the pin resolves to nothing installed
 */
@Service(Service.Level.PROJECT)
class JoltaSyncService(private val project: Project) : Disposable {
    private val log = Logger.getInstance(JoltaSyncService::class.java)
    private val syncing = AtomicBoolean(false)
    private var stampPoll: ScheduledFuture<*>? = null
    private var lastStamp: String? = null

    /** Last answer for the project root — always warm once the first sync lands. */
    @Volatile
    private var projectCurrent: JoltaCurrent? = null

    /** Per-directory answers for run configurations; dropped on every re-sync. */
    private val resolved = ConcurrentHashMap<String, JoltaCurrent>()

    fun start() {
        if (Jolta.binary() == null) {
            log.info("jolta binary not found — plugin idle")
            return
        }
        watchPinFiles()
        watchStamp()
        scheduleSync()
    }

    private fun projectDir(): File? = project.basePath?.let { File(it) }

    /* ---------- watching ---------- */

    private fun watchPinFiles() {
        project.messageBus.connect(this).subscribe(
            VirtualFileManager.VFS_CHANGES,
            object : BulkFileListener {
                override fun after(events: List<VFileEvent>) {
                    if (events.any { it.file?.name == ".java-version" || it.file?.name == ".sdkmanrc" }) {
                        scheduleSync()
                    }
                }
            },
        )
    }

    private fun watchStamp() {
        lastStamp = readStamp()
        // a 5s mtime poll: the stamp lives outside any content root, so the
        // VFS won't tell us about it, and one stat/5s is negligible
        stampPoll = AppExecutorUtil.getAppScheduledExecutorService().scheduleWithFixedDelay(
            {
                val s = readStamp()
                if (s != lastStamp) {
                    lastStamp = s
                    scheduleSync()
                }
            },
            5, 5, TimeUnit.SECONDS,
        )
    }

    private fun readStamp(): String? =
        try {
            Jolta.stampFile().takeIf { it.isFile }?.readText()
        } catch (e: Exception) {
            null
        }

    /* ---------- syncing ---------- */

    /**
     * A user-initiated sync always runs and always reports back; a reactive
     * one (watcher, poll) is dropped when another is already in flight and
     * stays quiet unless something actually changed.
     */
    fun scheduleSync(userInitiated: Boolean = false) {
        if (project.isDisposed) return
        if (!syncing.compareAndSet(false, true) && !userInitiated) return
        ApplicationManager.getApplication().executeOnPooledThread {
            try {
                syncNow(userInitiated)
            } finally {
                syncing.set(false)
            }
        }
    }

    private fun syncNow(userInitiated: Boolean) {
        val dir = projectDir() ?: return
        resolved.clear() // whatever woke us up may have changed every answer
        if (!Jolta.isGoverned(dir)) {
            projectCurrent = null
            JoltaStateListener.broadcast(project, null)
            offerToPin(dir, userInitiated)
            return
        }
        val current = Jolta.current(dir)
        if (current == null) {
            offerInstall(dir)
            return
        }
        projectCurrent = current
        resolved[dir.absolutePath] = current
        applyToIde(current, userInitiated)
    }

    /** The last resolved pin, for passive UI. Never blocks. */
    fun lastKnown(): JoltaCurrent? = projectCurrent

    /* ---------- resolution for run configurations ---------- */

    /**
     * The pin in effect for [workDir], for injecting into a run configuration.
     *
     * Answers from cache whenever it can. A miss costs a `jolta current` spawn,
     * so on the EDT we decline rather than block the UI — the project-level
     * answer is already warm and covers everything but a monorepo subdirectory
     * carrying its own pin.
     */
    fun currentFor(workDir: File?): JoltaCurrent? {
        val dir = workDir ?: projectDir() ?: return projectCurrent
        val key = dir.absolutePath
        resolved[key]?.let { return it }
        if (ApplicationManager.getApplication().isDispatchThread) return projectCurrent
        if (!Jolta.isGoverned(dir)) return projectCurrent
        val current = Jolta.current(dir, timeoutSec = 10) ?: return projectCurrent
        resolved[key] = current
        return current
    }

    /**
     * The project isn't pinned. If the IDE already has a working JDK selected,
     * that's the answer the user has effectively been living with — offer to
     * write it down so the terminal, CI, and teammates agree with the IDE.
     *
     * Deliberately an offer, not an action: creating a `.java-version` puts a
     * file in someone's repository, which is theirs to decide.
     */
    private fun offerToPin(dir: File, userInitiated: Boolean) {
        val state = project.service<JoltaProjectState>()
        if (state.suppressPinOffer && !userInitiated) return

        val sdk = ProjectRootManager.getInstance(project).projectSdk
        val sdkHome = sdk?.homePath
        // "configured without error": an SDK whose home is actually there.
        val healthy = sdk != null && sdkHome != null && File(sdkHome).isDirectory
        if (!healthy) {
            if (userInitiated) {
                notify(
                    "This project has no Java pin" +
                        (if (sdk == null) " and no project SDK" else " and its SDK '${sdk.name}' is missing from disk"),
                    NotificationType.WARNING,
                    chooseAction("Choose a JDK…"),
                )
            }
            return
        }

        val version = sdk.versionString?.let { Jolta.majorOf(it) }
            ?: Jolta.majorOf(File(sdkHome).name)
        if (version == null) {
            if (userInitiated) {
                notify(
                    "This project has no Java pin, and the version of SDK '${sdk.name}' couldn't be read",
                    NotificationType.WARNING,
                    chooseAction("Choose a JDK…"),
                )
            }
            return
        }

        notify(
            "This project isn't pinned. Pin it to Java $version so the terminal, CI, and your teammates " +
                "get the same JDK the IDE is using?",
            NotificationType.INFORMATION,
            NotificationAction.createSimpleExpiring("Pin Java $version") {
                pinInBackground(version.toString(), exact = false)
            },
            chooseAction("Choose a different JDK…"),
            NotificationAction.createSimpleExpiring("Don't ask for this project") {
                state.suppressPinOffer = true
            },
        )
    }

    private fun chooseAction(text: String): NotificationAction =
        NotificationAction.createSimpleExpiring(text) { JoltaPickJdkDialog.showAndPin(project) }

    /* ---------- pinning ---------- */

    /**
     * Write the pin, then re-sync. `jolta pin` downloads the JDK when nothing
     * installed matches, so this always runs behind a progress indicator.
     */
    fun pinInBackground(spec: String, exact: Boolean) {
        val dir = projectDir() ?: return
        object : Task.Backgroundable(project, "Jolta: pinning Java $spec", true) {
            override fun run(indicator: ProgressIndicator) {
                indicator.isIndeterminate = true
                indicator.text = "Writing .java-version and resolving $spec…"
                val result = Jolta.pin(dir, spec, exact)
                if (result.ok) {
                    resolved.clear() // the pin file changed under us
                    scheduleSync(userInitiated = true)
                } else {
                    notify("Could not pin Java $spec — ${result.message()}", NotificationType.ERROR)
                }
            }
        }.queue()
    }

    private fun offerInstall(dir: File) {
        if (!Jolta.pinFileExists(dir)) return // no pin: never surprise-install
        object : Task.Backgroundable(project, "Jolta: installing the pinned JDK", false) {
            override fun run(indicator: ProgressIndicator) {
                indicator.isIndeterminate = true
                if (Jolta.resolveInstalling(dir)) {
                    Jolta.current(dir)?.let { applyToIde(it) }
                } else {
                    notify(
                        "Jolta could not resolve this project's pin — run 'jolta doctor' in a terminal",
                        NotificationType.WARNING,
                    )
                }
            }
        }.queue()
    }

    /** See [JoltaOverride] for why a manual choice outranks a re-sync. */
    private fun isManuallyOverridden(current: JoltaCurrent): Boolean {
        val state = project.service<JoltaProjectState>()
        return JoltaOverride.isManualOverride(
            appliedSdkName = state.appliedSdkName,
            appliedPinKey = state.appliedPinKey,
            actualSdkName = ProjectRootManager.getInstance(project).projectSdk?.name,
            current = current,
        )
    }

    /** Non-null when the IDE is deliberately diverging from the pin. */
    @Volatile
    var divergence: String? = null
        private set

    private fun applyToIde(current: JoltaCurrent, userInitiated: Boolean = false) {
        val major = Jolta.majorOf(current.version) ?: return
        val name = "jolta ${current.vendor ?: "jdk"} $major"
        val state = project.service<JoltaProjectState>()

        ApplicationManager.getApplication().invokeLater {
            if (project.isDisposed) return@invokeLater

            if (!userInitiated && isManuallyOverridden(current)) {
                val actual = ProjectRootManager.getInstance(project).projectSdk?.name
                divergence = "Project SDK is '$actual', not the pinned ${current.version}"
                log.info("respecting a manual SDK override ($actual); pin is ${current.version}")
                JoltaStateListener.broadcast(project, current)
                return@invokeLater
            }
            divergence = null

            var replaced: String? = null
            var changed = false
            ApplicationManager.getApplication().runWriteAction {
                val sdk = ensureSdk(name, current.home)
                repairJoltaSdks()
                val rootManager = ProjectRootManager.getInstance(project)
                val previous = rootManager.projectSdk
                if (previous?.name != sdk.name) {
                    replaced = previous?.name
                    changed = true
                    rootManager.projectSdk = sdk
                }
                state.recordApplied(sdk.name, JoltaOverride.pinKey(current))
            }
            if (project.service<JoltaProjectState>().manageGradleAndMaven) {
                GradleApplier.apply(project, name)
                MavenApplier.apply(project, name)
            }
            JoltaStateListener.broadcast(project, current)

            val described = "${current.version} (${current.vendor ?: "unknown"})"
            val previousName = replaced
            when {
                // Taking over an SDK the user had set is the one case worth an
                // escape hatch — the pin still wins by default, but silently
                // overwriting a deliberate choice shouldn't be a one-way door.
                changed && previousName != null ->
                    notify(
                        "Project SDK set to $described — ${current.source ?: "jolta"} (was $previousName)",
                        NotificationType.INFORMATION,
                        undoTo(previousName),
                    )
                changed ->
                    notify("Project SDK set to $described — ${current.source ?: "jolta"}", NotificationType.INFORMATION)
                userInitiated ->
                    notify("Project SDK is already $described — ${current.source ?: "jolta"}", NotificationType.INFORMATION)
            }

            reportProblems(userInitiated)
        }
    }

    /** Problems already announced, so a stamp bump doesn't re-nag. */
    private val reported = java.util.Collections.synchronizedSet(mutableSetOf<String>())

    /**
     * Report only what is broken.
     *
     * Not "differs from the pin" — a Gradle toolchain, an explicit
     * `org.gradle.java.home`, or a hand-picked SDK all differ on purpose, and
     * warning about them is how a notification becomes something people learn
     * to dismiss. See [JoltaHealth].
     */
    private fun reportProblems(userInitiated: Boolean) {
        val dir = projectDir() ?: return
        val problems = JoltaHealth.check(project, dir)
        val ids = problems.map { it.id }.toSet()
        reported.retainAll(ids) // a problem that went away can be reported again

        for (problem in problems) {
            if (!reported.add(problem.id) && !userInitiated) continue
            notify(
                problem.message,
                NotificationType.WARNING,
                NotificationAction.createSimpleExpiring("Reapply the pin") {
                    scheduleSync(userInitiated = true)
                },
                NotificationAction.createSimpleExpiring("Choose a JDK…") {
                    JoltaPickJdkDialog.showAndPin(project)
                },
            )
        }
    }

    /** Restores [sdkName] as the project SDK, if it's still in the table. */
    private fun undoTo(sdkName: String): NotificationAction =
        NotificationAction.createSimpleExpiring("Undo") {
            val sdk = ProjectJdkTable.getInstance().findJdk(sdkName)
            if (sdk == null) {
                notify("SDK '$sdkName' is no longer registered", NotificationType.WARNING)
                return@createSimpleExpiring
            }
            ApplicationManager.getApplication().runWriteAction {
                ProjectRootManager.getInstance(project).projectSdk = sdk
            }
            GradleApplier.apply(project, sdkName)
            MavenApplier.apply(project, sdkName)
        }

    /** Find-or-create the named SDK; repoint it when an upgrade moved the home. */
    private fun ensureSdk(name: String, home: String): Sdk {
        val table = ProjectJdkTable.getInstance()
        val existing = table.findJdk(name)
        if (existing != null) {
            if (!FileUtil.pathsEqual(existing.homePath, home)) {
                repoint(existing, home)
            }
            return existing
        }
        val sdk = JavaSdk.getInstance().createJdk(name, home, false)
        table.addJdk(sdk)
        return sdk
    }

    /**
     * Any jolta-managed SDK whose install dir vanished (upgrade/prune renames
     * the versioned directory) gets repointed at the newest build of the same
     * major — the fix for IDE SDK entries rotting on every point release.
     */
    private fun repairJoltaSdks() {
        val jdksRoot = File(Jolta.joltaHome(), "jdks").absolutePath
        val stale = ProjectJdkTable.getInstance().allJdks.filter { sdk ->
            val home = sdk.homePath ?: return@filter false
            FileUtil.isAncestor(jdksRoot, home, false) && !File(home).isDirectory
        }
        if (stale.isEmpty()) return
        val installed = Jolta.jdks()
        for (sdk in stale) {
            val major = sdk.versionString?.let { Jolta.majorOf(it) }
                ?: sdk.name.takeLastWhile { it.isDigit() }.toIntOrNull()
                ?: continue
            val replacement = Jolta.newest(installed.filter { it.major == major })
            if (replacement != null) {
                repoint(sdk, replacement.home)
                log.info("repointed SDK '${sdk.name}' to ${replacement.home}")
            }
        }
    }

    private fun repoint(sdk: Sdk, home: String) {
        val mod = sdk.sdkModificator
        mod.homePath = home
        mod.commitChanges()
        sdk.sdkType.let { type ->
            if (type is JavaSdk) type.setupSdkPaths(sdk)
        }
    }

    private fun notify(text: String, type: NotificationType, vararg actions: NotificationAction) =
        JoltaNotifications.notify(project, text, type, *actions)

    override fun dispose() {
        stampPoll?.cancel(false)
    }
}
