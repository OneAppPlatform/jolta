package dev.jolta.intellij

import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationType
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.service
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.ide.CopyPasteManager
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.Messages
import java.awt.datatransfer.StringSelection
import java.io.File
import java.net.URI
import java.util.concurrent.TimeUnit

/**
 * Offers to install the jolta CLI when it isn't there.
 *
 * Two rules shape this:
 *
 *  - **Never silently.** The user is shown the exact command and has to accept
 *    it. A plugin that quietly runs an installer off the internet has earned
 *    every bit of suspicion that follows.
 *  - **Never hidden.** The command runs in the IDE's own terminal, so its
 *    output is visible, scrollable, and interruptible with Ctrl-C — not buried
 *    in a progress bar the user can't inspect.
 */
object JoltaInstaller {
    private val log = Logger.getInstance(JoltaInstaller::class.java)

    const val CURL_COMMAND =
        "curl -fsSL https://raw.githubusercontent.com/OneAppPlatform/jolta/main/install.sh | sh"

    const val RELEASES_URL = "https://github.com/OneAppPlatform/jolta/releases"
    const val GUIDE_URL = "https://oneappplatform.github.io/jolta/"

    private val windows = System.getProperty("os.name", "").startsWith("Windows")

    /**
     * The installer is a POSIX shell script. On Windows there's no equivalent
     * one-liner yet, so we send people to the releases page rather than hand
     * them a command that won't run in PowerShell.
     */
    fun canInstallHere(): Boolean = !windows

    /** The notification shown wherever we discover jolta is missing. */
    fun offer(project: Project, reason: String) {
        if (!canInstallHere()) {
            JoltaNotifications.notify(
                project,
                "$reason Download it from the releases page and run `jolta.exe setup`.",
                NotificationType.WARNING,
                NotificationAction.createSimpleExpiring("Open releases page") { browse(RELEASES_URL) },
            )
            return
        }
        JoltaNotifications.notify(
            project,
            "$reason Install it and this project's Java version will be managed for you.",
            NotificationType.INFORMATION,
            NotificationAction.createSimpleExpiring("Install jolta…") { confirmAndInstall(project) },
            NotificationAction.createSimpleExpiring("Copy install command") {
                CopyPasteManager.getInstance().setContents(StringSelection(CURL_COMMAND))
                JoltaNotifications.notify(project, "Install command copied to the clipboard", NotificationType.INFORMATION)
            },
            NotificationAction.createSimpleExpiring("Install guide") { browse(GUIDE_URL) },
        )
    }

    /**
     * Show the command, get an explicit yes, then run it in a terminal tab.
     */
    fun confirmAndInstall(project: Project) {
        val choice = Messages.showOkCancelDialog(
            project,
            "This will run the following command in a terminal:\n\n" +
                "    $CURL_COMMAND\n\n" +
                "It downloads a prebuilt jolta binary, installs it into ~/.jolta, and adds two marked " +
                "blocks to your shell profile so new shells pick it up. Nothing outside ~/.jolta and your " +
                "profile is touched, and `jolta implode` undoes all of it.\n\n" +
                "The command runs in the IDE terminal so you can watch it and stop it at any point.",
            "Install the jolta CLI?",
            "Run It",
            "Cancel",
            Messages.getQuestionIcon(),
        )
        if (choice != Messages.OK) return

        if (!runInTerminal(project, CURL_COMMAND)) {
            CopyPasteManager.getInstance().setContents(StringSelection(CURL_COMMAND))
            JoltaNotifications.notify(
                project,
                "Couldn't open a terminal here — the install command has been copied to your clipboard, " +
                    "run it in any shell.",
                NotificationType.WARNING,
            )
            return
        }
        awaitInstall(project)
    }

    /**
     * The terminal plugin is optional (and absent in some IDEs), and the JVM
     * resolves these classes lazily at first call, so the guard is enough.
     */
    private fun runInTerminal(project: Project, command: String): Boolean =
        try {
            val manager = org.jetbrains.plugins.terminal.TerminalToolWindowManager.getInstance(project)
            val widget = manager.createShellWidget(project.basePath, "Install jolta", true, true)
            widget.sendCommandToExecute(command)
            true
        } catch (e: NoClassDefFoundError) {
            log.info("terminal plugin unavailable — falling back to clipboard")
            false
        } catch (e: Exception) {
            log.warn("cannot open a terminal for the jolta install", e)
            false
        }

    /**
     * Watch for the binary to appear rather than guessing how long a download
     * takes. The install is happening in a terminal we don't control, so this
     * polls and then starts the sync the user was originally after.
     */
    private fun awaitInstall(project: Project) {
        object : Task.Backgroundable(project, "Jolta: waiting for the install to finish", true) {
            override fun run(indicator: ProgressIndicator) {
                indicator.isIndeterminate = true
                // Name the path being watched. If the install lands somewhere
                // else — a different JOLTA_HOME in the shell that ran it — this
                // otherwise looks like a hang with nothing to go on.
                indicator.text = "Watching for ${expectedBinaryPath()}"
                val deadline = System.nanoTime() + TimeUnit.MINUTES.toNanos(5)
                while (System.nanoTime() < deadline) {
                    if (indicator.isCanceled || project.isDisposed) return
                    if (Jolta.binary() != null) {
                        onInstalled(project)
                        return
                    }
                    Thread.sleep(1000)
                }
                // The commonest cause of landing here: the install ran in a
                // shell with a different JOLTA_HOME, so it's on disk but not
                // where this IDE looks. Say so, rather than just "not found".
                val elsewhere = File(File(System.getProperty("user.home"), ".jolta"), "bin")
                    .resolve(if (windows) "jolta.exe" else "jolta")
                val hint = if (Jolta.homeOverride == null && System.getenv("JOLTA_HOME") != null &&
                    elsewhere.canExecute()
                ) {
                    " There is one at $elsewhere, but this IDE is using JOLTA_HOME=" +
                        "${System.getenv("JOLTA_HOME")}."
                } else {
                    ""
                }
                JoltaNotifications.notify(
                    project,
                    "Still no jolta at ${expectedBinaryPath()}.$hint If the install finished, use " +
                        "Tools → Jolta → Reload JDK.",
                    NotificationType.WARNING,
                )
            }
        }.queue()
    }

    private fun onInstalled(project: Project) {
        JoltaNotifications.notify(
            project,
            "jolta is installed. Setting up this project's Java version now.",
            NotificationType.INFORMATION,
        )
        ApplicationManager.getApplication().invokeLater {
            if (project.isDisposed) return@invokeLater
            project.service<JoltaSyncService>().start()
            project.service<JoltaSyncService>().scheduleSync(userInitiated = true)
        }
    }

    private fun browse(url: String) {
        runCatching { com.intellij.ide.BrowserUtil.browse(URI(url)) }
            .onFailure { log.warn("cannot open $url", it) }
    }

    /** Where the CLI would land — used in messages so they're never vague. */
    fun expectedBinaryPath(): File = File(File(Jolta.joltaHome(), "bin"), if (windows) "jolta.exe" else "jolta")
}
