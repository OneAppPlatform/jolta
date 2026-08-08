package dev.jolta.intellij

import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.DataContext
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.actionSystem.Separator
import com.intellij.openapi.components.service
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.popup.JBPopupFactory
import com.intellij.openapi.ui.popup.ListPopup
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.openapi.wm.StatusBarWidget
import com.intellij.openapi.wm.StatusBarWidgetFactory
import com.intellij.openapi.wm.impl.status.EditorBasedStatusBarPopup
import kotlinx.coroutines.CoroutineScope

/**
 * The status bar entry: what Java this project is on, and one click to change it.
 *
 * A version manager only earns its keep if the answer is visible without going
 * looking for it. This is also where "refresh" lives — the pin can change from
 * a terminal, a teammate's commit, or `jolta upgrade`, and a visible control to
 * re-read it beats wondering whether the IDE noticed.
 */
class JoltaStatusBarWidget(project: Project, private val widgetScope: CoroutineScope) :
    EditorBasedStatusBarPopup(project, false, widgetScope) {

    init {
        project.messageBus.connect(this).subscribe(
            JoltaStateListener.TOPIC,
            JoltaStateListener { update(null) },
        )
    }

    override fun ID(): String = WIDGET_ID

    override fun createInstance(project: Project): StatusBarWidget = JoltaStatusBarWidget(project, widgetScope)

    override fun getWidgetState(file: VirtualFile?): WidgetState {
        if (Jolta.binary() == null) {
            return WidgetState("jolta CLI not found — click for help", "Java: no jolta", true)
        }
        val current = project.service<JoltaSyncService>().lastKnown()
            ?: return WidgetState(
                "This project has no Java pin. Click to set one.",
                "Java: unpinned",
                true,
            )
        val major = Jolta.majorOf(current.version) ?: return WidgetState.HIDDEN
        val vendor = current.vendor?.let { " ($it)" } ?: ""

        // A deliberate divergence is shown, not announced: visible if you look,
        // silent if you don't.
        val divergence = project.service<JoltaSyncService>().divergence
        if (divergence != null) {
            return WidgetState(
                "$divergence.\nYour choice is being kept — the pin will apply again when it changes.\n" +
                    "Click to reapply it now.",
                "Java $major$vendor \u26A0",
                true,
            )
        }

        return WidgetState(
            "Java ${current.version}$vendor\nPinned by ${current.source ?: "jolta"}\nClick to change",
            "Java $major$vendor",
            true,
        )
    }

    override fun createPopup(context: DataContext): ListPopup {
        val service = project.service<JoltaSyncService>()
        val group = DefaultActionGroup()

        if (Jolta.binary() == null) {
            group.add(object : DumbAwareAction("Install the jolta CLI…") {
                override fun getActionUpdateThread() = ActionUpdateThread.BGT
                override fun actionPerformed(e: AnActionEvent) =
                    JoltaInstaller.offer(project, "jolta isn't installed yet.")
            })
            return JBPopupFactory.getInstance().createActionGroupPopup(
                "Java Version", group, context, JBPopupFactory.ActionSelectionAid.SPEEDSEARCH, false,
            )
        }

        // The handful of installed JDKs go straight in the popup — switching
        // between the two majors a team actually uses shouldn't need a dialog.
        val current = service.lastKnown()
        val installed = Jolta.jdks()
            .sortedWith(
                compareByDescending<JoltaJdk> { it.major }
                    .thenByDescending(Comparator<String> { a, b -> Jolta.compareVersions(a, b) }) { it.version },
            )
            .distinctBy { it.major to it.vendor }
            .take(8)

        for (jdk in installed) {
            val active = current != null && Jolta.majorOf(current.version) == jdk.major &&
                (current.vendor == null || current.vendor == jdk.vendor)
            group.add(object : DumbAwareAction(
                "${if (active) "✓ " else ""}Java ${jdk.version} (${jdk.vendor})",
            ) {
                override fun getActionUpdateThread() = ActionUpdateThread.BGT
                override fun actionPerformed(e: AnActionEvent) = JoltaPickJdkDialog.pinDirectly(project, jdk)
            })
        }

        if (installed.isNotEmpty()) group.add(Separator.getInstance())

        group.add(object : DumbAwareAction("Set Java Version…") {
            override fun getActionUpdateThread() = ActionUpdateThread.BGT
            override fun actionPerformed(e: AnActionEvent) = JoltaPickJdkDialog.showAndPin(project)
        })
        if (service.divergence != null) {
            group.add(object : DumbAwareAction("Reapply the pin") {
                override fun getActionUpdateThread() = ActionUpdateThread.BGT
                override fun actionPerformed(e: AnActionEvent) = service.scheduleSync(userInitiated = true)
            })
        }
        group.add(object : DumbAwareAction("Refresh from jolta") {
            override fun getActionUpdateThread() = ActionUpdateThread.BGT
            override fun actionPerformed(e: AnActionEvent) {
                service.scheduleSync(userInitiated = true)
            }
        })

        return JBPopupFactory.getInstance().createActionGroupPopup(
            "Java Version",
            group,
            context,
            JBPopupFactory.ActionSelectionAid.SPEEDSEARCH,
            false,
        )
    }

    /**
     * What the status bar would render: label and tooltip. WidgetState is
     * protected platform API, so tests go through this rather than widening it.
     */
    fun renderForTest(): Pair<String?, String?> =
        getWidgetState(null).let { it.text to it.toolTip }

    companion object {
        const val WIDGET_ID = "dev.jolta.statusBar"
    }
}

class JoltaStatusBarWidgetFactory : StatusBarWidgetFactory, DumbAware {
    override fun getId(): String = JoltaStatusBarWidget.WIDGET_ID

    override fun getDisplayName(): String = "Java Version (jolta)"

    // The scope-based overload is the current contract; the widget's coroutines
    // are then tied to the status bar's lifetime rather than ours to manage.
    override fun createWidget(project: Project, scope: CoroutineScope): StatusBarWidget =
        JoltaStatusBarWidget(project, scope)

    // Shown even without the CLI: "Java: no jolta" is where the install offer
    // lives, and a widget that vanishes when you most need it is no help.
    override fun isAvailable(project: Project): Boolean = true
}
