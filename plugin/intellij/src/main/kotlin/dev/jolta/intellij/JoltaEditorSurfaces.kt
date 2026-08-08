package dev.jolta.intellij

import com.intellij.codeInsight.daemon.ProjectSdkSetupValidator
import com.intellij.openapi.components.service
import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.roots.ProjectRootManager
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.ui.EditorNotificationPanel
import com.intellij.ui.EditorNotificationProvider
import java.io.File
import java.util.function.Function
import javax.swing.JComponent

/**
 * A banner across the top of a `.java-version` file.
 *
 * The pin is a one-line text file with no syntax highlighting and no feedback —
 * you find out it's wrong when `java` stops working somewhere else. This says
 * what the line currently resolves to, right where it is being edited, and
 * offers the fix when it resolves to nothing.
 */
class JoltaPinFileNotificationProvider : EditorNotificationProvider, DumbAware {

    override fun collectNotificationData(
        project: Project,
        file: VirtualFile,
    ): Function<in FileEditor, out JComponent?>? {
        if (file.name != ".java-version" && file.name != ".sdkmanrc") return null
        if (Jolta.binary() == null) {
            return Function { editor ->
                EditorNotificationPanel(editor, EditorNotificationPanel.Status.Warning).apply {
                    text = "The jolta CLI isn't installed, so this pin isn't being applied."
                    createActionLabel("Install jolta…") { JoltaInstaller.offer(project, "jolta isn't installed.") }
                }
            }
        }

        val spec = readSpec(file) ?: return null
        if (!Jolta.isPinnable(spec)) {
            return Function { editor ->
                EditorNotificationPanel(editor, EditorNotificationPanel.Status.Error).apply {
                    text = "\"$spec\" isn't a version jolta can resolve — every java command below this " +
                        "directory will fail until it is fixed."
                    createActionLabel("Choose a version…") { JoltaPickJdkDialog.showAndPin(project) }
                }
            }
        }

        val current = project.service<JoltaSyncService>().lastKnown()
        return Function { editor ->
            EditorNotificationPanel(editor, EditorNotificationPanel.Status.Info).apply {
                text = if (current != null) {
                    "Resolves to Java ${current.version}${current.vendor?.let { " ($it)" } ?: ""}"
                } else {
                    "Not resolved yet — no installed JDK matches \"$spec\""
                }
                createActionLabel("Change…") { JoltaPickJdkDialog.showAndPin(project) }
                createActionLabel("Reload") {
                    project.service<JoltaSyncService>().scheduleSync(userInitiated = true)
                }
            }
        }
    }

    /** First non-blank, non-comment line — the same rule the CLI uses. */
    private fun readSpec(file: VirtualFile): String? =
        try {
            String(file.contentsToByteArray(), Charsets.UTF_8)
                .lineSequence()
                .map { it.trim() }
                .firstOrNull { it.isNotEmpty() && !it.startsWith("#") }
        } catch (e: Exception) {
            null
        }
}

/**
 * Hooks the IDE's own "Project SDK is not defined" banner.
 *
 * That banner is where people already look when Java isn't working — it appears
 * above the source file they're staring at, not in a balloon they may have
 * dismissed. When the project is pinned, we can both explain why the SDK is
 * missing and fix it from there.
 */
class JoltaSdkSetupValidator : ProjectSdkSetupValidator {

    override fun isApplicableFor(project: Project, file: VirtualFile): Boolean {
        if (file.extension != "java") return false
        val dir = project.basePath?.let(::File) ?: return false
        // Only speak up where jolta is actually in charge; an unpinned project's
        // missing SDK is the IDE's business, not ours.
        return Jolta.binary() != null && Jolta.isGoverned(dir)
    }

    override fun getErrorMessage(project: Project, file: VirtualFile): String? {
        val sdk = ProjectRootManager.getInstance(project).projectSdk
        val current = project.service<JoltaSyncService>().lastKnown()
        return when {
            sdk == null && current != null ->
                "This project pins Java ${current.version} but no Project SDK is set"
            sdk == null ->
                "This project has a Java pin that hasn't been applied yet"
            sdk.homePath?.let { File(it).isDirectory } == false ->
                "The Project SDK '${sdk.name}' is missing from disk"
            else -> null // healthy: stay out of the way
        }
    }

    override fun getFixHandler(project: Project, file: VirtualFile): EditorNotificationPanel.ActionHandler =
        object : EditorNotificationPanel.ActionHandler {
            override fun handlePanelActionClick(panel: EditorNotificationPanel, event: javax.swing.event.HyperlinkEvent) =
                fix()

            override fun handleQuickFixClick(editor: com.intellij.openapi.editor.Editor, psiFile: com.intellij.psi.PsiFile) =
                fix()

            private fun fix() {
                project.service<JoltaSyncService>().scheduleSync(userInitiated = true)
            }
        }
}
