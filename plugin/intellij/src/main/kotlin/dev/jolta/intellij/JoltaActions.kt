package dev.jolta.intellij

import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.components.service
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.project.Project

/**
 * Every action first has to answer "is jolta even here?". Without the CLI the
 * plugin has nothing to resolve with, and a dead menu item that silently does
 * nothing is worse than one that offers the way out.
 */
private fun requireJolta(project: Project): Boolean {
    if (Jolta.binary() != null) return true
    JoltaInstaller.offer(
        project,
        "jolta isn't installed — the plugin looks for it at ${JoltaInstaller.expectedBinaryPath()}, " +
            "not on PATH, so that IDEs launched from the Dock work too.",
    )
    return false
}

/**
 * Tools → Jolta → Set Java Version…
 *
 * The one action that both picks a JDK and pins it: choosing a version in the
 * IDE and having the terminal disagree is the problem this plugin exists to
 * remove, so there is deliberately no "just change the IDE" option here.
 */
class JoltaSetVersionAction : DumbAwareAction() {
    override fun getActionUpdateThread() = ActionUpdateThread.BGT

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        if (!requireJolta(project)) return
        JoltaPickJdkDialog.showAndPin(project)
    }
}

/**
 * Tools → Jolta → Reload JDK.
 *
 * The watchers cover pin edits and anything that bumps jolta's stamp, but an
 * SDK can still drift out from under the IDE — a JDK moved by hand, a sync that
 * lost a race, an Undo the user wants to take back. Unlike a reactive sync this
 * always reports what it found, so "nothing changed" is an answer.
 */
class JoltaReloadAction : DumbAwareAction() {
    override fun getActionUpdateThread() = ActionUpdateThread.BGT

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        if (!requireJolta(project)) return
        project.service<JoltaSyncService>().scheduleSync(userInitiated = true)
    }
}
