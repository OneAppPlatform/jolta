package dev.jolta.intellij

import com.intellij.openapi.components.service
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.openapi.ui.ValidationInfo
import com.intellij.ui.SimpleListCellRenderer
import com.intellij.ui.SimpleTextAttributes
import com.intellij.ui.components.JBList
import com.intellij.ui.dsl.builder.AlignX
import com.intellij.ui.dsl.builder.bindSelected
import com.intellij.ui.dsl.builder.bindText
import com.intellij.ui.dsl.builder.panel
import com.intellij.ui.components.JBScrollPane
import javax.swing.DefaultListModel
import javax.swing.JComponent
import javax.swing.JList
import javax.swing.ListSelectionModel

/**
 * Pick a JDK and pin the project to it.
 *
 * The list is whatever `jolta jdks` reports — jolta-managed installs and the
 * system JDKs it discovered. The text field exists because jolta will fetch
 * what it doesn't have: typing `25` when the list has no 25 is a valid answer,
 * not an error, which is the whole reason a version manager is here.
 *
 * Layout is Kotlin UI DSL v2, so spacing, label alignment, and comment styling
 * follow the platform rather than this plugin's guesses about them.
 */
class JoltaPickJdkDialog(
    project: Project,
    private val installed: List<JoltaJdk>,
    private val initial: JoltaPinRequest,
) : DialogWrapper(project, true) {

    private val listModel = DefaultListModel<JoltaJdk>()
    private val list = JBList(listModel)

    /** The DSL panel, kept so tests can inspect what was actually built. */
    lateinit var centerPanel: com.intellij.openapi.ui.DialogPanel
        private set

    /** Mutable model the DSL binds to; also what tests drive. */
    val request: JoltaPinRequest = initial.copy()

    init {
        title = "Set the Java Version for This Project"
        setOKButtonText("Pin")
        sortedJdks().forEach(listModel::addElement)
        init()
    }

    /** The list order as rendered — exposed so tests can assert on it. */
    fun orderedForTest(): List<JoltaJdk> = sortedJdks()

    /** Newest major first, newest build within it. */
    private fun sortedJdks(): List<JoltaJdk> = installed.sortedWith(
        compareByDescending<JoltaJdk> { it.major }
            .thenByDescending(Comparator<String> { a, b -> Jolta.compareVersions(a, b) }) { it.version }
            .thenBy { it.vendor },
    )

    override fun createCenterPanel(): JComponent {
        configureList()
        centerPanel = panel {
            row {
                cell(JBScrollPane(list))
                    .align(AlignX.FILL)
                    .comment("Installed and discovered by jolta")
                    .resizableColumn()
            }.resizableRow()

            row("Version:") {
                textField()
                    .bindText(request::spec)
                    .align(AlignX.FILL)
                    .comment(
                        "Any spec jolta accepts — <code>21</code>, <code>corretto@21</code>, " +
                            "<code>lts</code>, <code>21.0.4</code>. It downloads what you don't have.",
                    )
                    .validationOnInput { field ->
                        // Mirror the field into the model so validation and OK
                        // both see what is actually typed, not the last binding.
                        request.spec = field.text
                        request.validate()?.let { ValidationInfo(it, field) }
                    }
            }

            row {
                checkBox("Pin the exact point release")
                    .bindSelected(request::exact)
                    .comment(
                        "Writes the exact build you have (for example 21.0.11) instead of just the " +
                            "major (21), so CI and teammates can't drift onto a different patch.",
                    )
            }
        }
        return centerPanel
    }

    private fun configureList() {
        list.selectionMode = ListSelectionModel.SINGLE_SELECTION
        list.visibleRowCount = 8
        // Subclassed rather than SimpleListCellRenderer.create(...), which is
        // scheduled for removal as of 2026.2. customize() is the stable API and
        // exists all the way back to our sinceBuild.
        list.cellRenderer = object : SimpleListCellRenderer<JoltaJdk>() {
            override fun customize(
                list: JList<out JoltaJdk>,
                value: JoltaJdk?,
                index: Int,
                selected: Boolean,
                hasFocus: Boolean,
            ) {
                text = value?.let { "Java ${it.version}  ·  ${it.vendor}" }.orEmpty()
                toolTipText = value?.home
            }
        }
        list.emptyText.text = "No JDKs installed yet"
        list.emptyText.appendSecondaryText(
            "Type a version below — jolta will download it",
            SimpleTextAttributes.GRAYED_ATTRIBUTES,
            null,
        )
        list.addListSelectionListener { e ->
            if (e.valueIsAdjusting) return@addListSelectionListener
            list.selectedValue?.let { selectSpec(Jolta.specFor(it.vendor, it.major)) }
        }
        preselect()
    }

    /** Selecting in the list is just another way of typing a spec. */
    private fun selectSpec(spec: String) {
        request.spec = spec
        // The bound field reads from the model on reset().
        if (::centerPanel.isInitialized) centerPanel.reset()
    }

    private fun preselect() {
        val spec = initial.normalized().takeIf { it.isNotEmpty() } ?: return
        listModel.elements().toList()
            .indexOfFirst { Jolta.specFor(it.vendor, it.major) == spec }
            .takeIf { it >= 0 }
            ?.let {
                list.selectedIndex = it
                list.ensureIndexIsVisible(it)
            }
    }

    override fun getPreferredFocusedComponent(): JComponent? =
        if (listModel.isEmpty) super.getPreferredFocusedComponent() else list

    /**
     * Belt and braces: `validationOnInput` guides as you type, this refuses to
     * close on a bad value. An unparseable `.java-version` breaks every java
     * invocation below this directory, including the shell you'd fix it from.
     */
    override fun doValidate(): ValidationInfo? =
        validationMessage()?.let { ValidationInfo(it) }

    /** The message the OK button would refuse on, or null. Public for tests. */
    fun validationMessage(): String? = request.validate()

    /** What the caller should actually pin. */
    fun spec(): String = request.normalized()

    fun exact(): Boolean = request.exact

    companion object {
        /** Load the JDK list off the EDT, then show the dialog. Pins on OK. */
        fun showAndPin(project: Project) {
            object : Task.Backgroundable(project, "Jolta: looking for installed JDKs", false) {
                private var jdks: List<JoltaJdk> = emptyList()

                override fun run(indicator: ProgressIndicator) {
                    indicator.isIndeterminate = true
                    jdks = Jolta.jdks()
                }

                override fun onSuccess() {
                    if (project.isDisposed) return
                    val service = project.service<JoltaSyncService>()
                    val initial = service.lastKnown()
                        ?.let { c -> Jolta.majorOf(c.version)?.let { JoltaPinRequest(Jolta.specFor(c.vendor, it)) } }
                        ?: JoltaPinRequest()
                    val dialog = JoltaPickJdkDialog(project, jdks, initial)
                    if (dialog.showAndGet()) {
                        service.pinInBackground(dialog.spec(), dialog.exact())
                    }
                }
            }.queue()
        }

        /** Pin straight to a known JDK, skipping the dialog (status bar popup). */
        fun pinDirectly(project: Project, jdk: JoltaJdk) {
            val request = JoltaPinRequest.forJdk(jdk)
            project.service<JoltaSyncService>().pinInBackground(request.normalized(), request.exact)
        }
    }
}
