package dev.jolta.intellij

import com.intellij.openapi.components.service
import com.intellij.openapi.options.BoundSearchableConfigurable
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.DialogPanel
import com.intellij.ui.dsl.builder.bindSelected
import com.intellij.ui.dsl.builder.panel

/**
 * Settings → Tools → Jolta.
 *
 * Everything here is an off switch. The defaults are what the plugin is for, so
 * a settings page shouldn't be needed to get value out of it — but "this tool
 * writes to my run configurations" is a reasonable thing to want to stop, and
 * an integration with no way to decline it is one people uninstall instead.
 */
class JoltaConfigurable(private val project: Project) :
    BoundSearchableConfigurable("Jolta", "dev.jolta.settings") {

    override fun createPanel(): DialogPanel {
        val state = project.service<JoltaProjectState>()
        return panel {
            group("This Project") {
                row {
                    checkBox("Set the Gradle JVM and Maven importer JDK from the pin")
                        .bindSelected(state::manageGradleAndMaven)
                        .comment(
                            "Off means only the Project SDK follows the pin. Gradle and Maven keep whatever " +
                                "JVM they are configured with, which is usually how imports end up on a " +
                                "different JDK from the editor.",
                        )
                }
                row {
                    checkBox("Set JAVA_HOME and PATH on Java run configurations")
                        .bindSelected(state::injectRunConfigEnv)
                        .comment(
                            "The JVM a run configuration launches is already the Project SDK. This covers what " +
                                "it shells out to — a test that runs a Gradle build, a process that reads " +
                                "<code>JAVA_HOME</code>. A value you set on the run configuration itself always wins.",
                        )
                }
                row {
                    checkBox("Give modules with their own pin their own JDK")
                        .bindSelected(state::manageModuleSdks)
                        .comment(
                            "A module with its own <code>.java-version</code> gets its own SDK instead of " +
                                "inheriting the project's — the same nearest-pin-wins rule the terminal uses. " +
                                "In Gradle and Maven projects the build system owns module SDKs and will " +
                                "overwrite this on re-import; a toolchain is the durable way to say it there.",
                        )
                }
                row {
                    checkBox("Offer to pin this project when it isn't pinned")
                        .bindSelected(
                            getter = { !state.suppressPinOffer },
                            setter = { state.suppressPinOffer = !it },
                        )
                        .comment("Re-enables the offer if you previously chose \"Don't ask for this project\".")
                }
            }
            group("jolta CLI") {
                row("Location:") {
                    label(
                        Jolta.binary()?.absolutePath
                            ?: "not installed — expected at ${JoltaInstaller.expectedBinaryPath()}",
                    )
                }
                row {
                    comment(
                        "The plugin looks for jolta at <code>\$JOLTA_HOME/bin</code> rather than on " +
                            "<code>PATH</code>, so an IDE launched from the Dock or Start menu — which never " +
                            "sees your shell profile — still finds it.",
                    )
                }
            }
        }
    }
}
