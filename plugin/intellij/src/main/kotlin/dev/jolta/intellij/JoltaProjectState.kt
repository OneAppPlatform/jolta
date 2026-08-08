package dev.jolta.intellij

import com.intellij.openapi.components.PersistentStateComponent
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import com.intellij.openapi.project.Project
import com.intellij.util.messages.Topic

/**
 * The little that has to outlive a session.
 *
 * Only one thing so far: whether the user told us to stop offering to pin this
 * project. An offer that reappears every time the IDE starts is nagging, and
 * "no" has to mean no.
 */
@Service(Service.Level.PROJECT)
@State(name = "JoltaProjectState", storages = [Storage("jolta.xml")])
class JoltaProjectState : PersistentStateComponent<JoltaProjectState.State> {
    data class State(
        var suppressPinOffer: Boolean = false,
        var suppressInstallOffer: Boolean = false,
        var injectRunConfigEnv: Boolean = true,
        var manageGradleAndMaven: Boolean = true,
        /** The SDK name this plugin last set, so a later change can be attributed. */
        var appliedSdkName: String? = null,
        /** The resolved pin it was set for; when this moves, the pin wins again. */
        var appliedPinKey: String? = null,
    )

    private var state = State()

    override fun getState(): State = state

    override fun loadState(loaded: State) {
        state = loaded
    }

    /** The user declined to pin this project; stop asking. */
    var suppressPinOffer: Boolean
        get() = state.suppressPinOffer
        set(value) {
            state.suppressPinOffer = value
        }

    /** The install offer is made once per project, not once per startup. */
    var suppressInstallOffer: Boolean
        get() = state.suppressInstallOffer
        set(value) {
            state.suppressInstallOffer = value
        }

    /**
     * Put JAVA_HOME/PATH on Java run configurations. On by default: the JVM a
     * run configuration launches is already the project SDK, but anything it
     * shells out to would otherwise pick up whatever the IDE was started with.
     */
    var injectRunConfigEnv: Boolean
        get() = state.injectRunConfigEnv
        set(value) {
            state.injectRunConfigEnv = value
        }

    /** Also point the Gradle JVM and Maven importer at the pin. */
    var manageGradleAndMaven: Boolean
        get() = state.manageGradleAndMaven
        set(value) {
            state.manageGradleAndMaven = value
        }

    var appliedSdkName: String?
        get() = state.appliedSdkName
        set(value) {
            state.appliedSdkName = value
        }

    var appliedPinKey: String?
        get() = state.appliedPinKey
        set(value) {
            state.appliedPinKey = value
        }

    fun recordApplied(sdkName: String, pinKey: String) {
        state.appliedSdkName = sdkName
        state.appliedPinKey = pinKey
    }
}

/** Fired whenever the resolved pin changes, so passive UI can redraw. */
fun interface JoltaStateListener {
    fun joltaStateChanged(current: JoltaCurrent?)

    companion object {
        val TOPIC: Topic<JoltaStateListener> = Topic.create("Jolta state", JoltaStateListener::class.java)

        fun broadcast(project: Project, current: JoltaCurrent?) {
            if (project.isDisposed) return
            project.messageBus.syncPublisher(TOPIC).joltaStateChanged(current)
        }
    }
}
