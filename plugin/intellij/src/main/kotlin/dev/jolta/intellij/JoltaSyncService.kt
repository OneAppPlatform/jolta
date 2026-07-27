package dev.jolta.intellij

import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
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

    fun scheduleSync() {
        if (project.isDisposed || !syncing.compareAndSet(false, true)) return
        ApplicationManager.getApplication().executeOnPooledThread {
            try {
                syncNow()
            } finally {
                syncing.set(false)
            }
        }
    }

    private fun syncNow() {
        val dir = projectDir() ?: return
        val current = Jolta.current(dir)
        if (current == null) {
            offerInstall(dir)
            return
        }
        applyToIde(current)
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

    private fun applyToIde(current: JoltaCurrent) {
        val major = Jolta.majorOf(current.version) ?: return
        val name = "jolta ${current.vendor ?: "jdk"} $major"
        ApplicationManager.getApplication().invokeLater {
            if (project.isDisposed) return@invokeLater
            ApplicationManager.getApplication().runWriteAction {
                val sdk = ensureSdk(name, current.home)
                repairJoltaSdks()
                val rootManager = ProjectRootManager.getInstance(project)
                if (rootManager.projectSdk?.name != sdk.name) {
                    rootManager.projectSdk = sdk
                    notify(
                        "Project SDK set to ${current.version} (${current.vendor ?: "unknown"}) — ${current.source ?: "jolta"}",
                        NotificationType.INFORMATION,
                    )
                }
            }
            GradleApplier.apply(project, name)
            MavenApplier.apply(project, name)
        }
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
            val replacement = installed.filter { it.major == major }.maxByOrNull { it.version }
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

    private fun notify(text: String, type: NotificationType) {
        NotificationGroupManager.getInstance()
            .getNotificationGroup("Jolta")
            .createNotification(text, type)
            .notify(project)
    }

    override fun dispose() {
        stampPoll?.cancel(false)
    }
}
