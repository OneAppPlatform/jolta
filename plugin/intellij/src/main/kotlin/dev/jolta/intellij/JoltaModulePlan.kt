package dev.jolta.intellij

/**
 * Which modules need their own SDK, and which should go back to inheriting.
 *
 * "The most specific pin wins" is jolta's core rule, and it doesn't stop at the
 * project boundary: a module with its own `.java-version` compiles against that
 * JDK, exactly as the terminal would in that directory.
 *
 * The half that's easy to forget is the reverse. A module that *had* its own pin
 * and lost it — file deleted, or the project pin moved to match — must go back
 * to inheriting the project SDK. Without that it keeps a module SDK nobody asked
 * for, and no amount of editing `.java-version` ever clears it. So the plan is
 * computed against what was previously applied, not just what's wanted now.
 *
 * Pure on purpose: this governs when the plugin overwrites module configuration,
 * which is not something to leave only testable through a live IDE.
 */
object JoltaModulePlan {

    /**
     * @param assign module name -> the pin it should be set to
     * @param revert modules to hand back to the project SDK
     */
    data class Plan(
        val assign: Map<String, JoltaCurrent>,
        val revert: Set<String>,
    ) {
        val isEmpty: Boolean get() = assign.isEmpty() && revert.isEmpty()
    }

    /**
     * @param projectPin       what the project root resolves to
     * @param resolvedByModule every module's own resolution, whether or not it differs
     * @param previouslyApplied modules this plugin has given an SDK to before
     * @param knownModules     modules that currently exist in the project
     */
    fun compute(
        projectPin: JoltaCurrent,
        resolvedByModule: Map<String, JoltaCurrent>,
        previouslyApplied: Set<String>,
        knownModules: Set<String> = resolvedByModule.keys,
    ): Plan {
        val projectKey = JoltaOverride.pinKey(projectPin)

        // Only modules that genuinely differ get their own SDK. A module
        // resolving to the same pin as the project inherits it — a redundant
        // module SDK is one more thing to go stale after `jolta upgrade`.
        val assign = resolvedByModule.filterValues { JoltaOverride.pinKey(it) != projectKey }

        // Anything we set before that no longer needs it goes back to
        // inheriting — but only if the module still exists. A module that was
        // removed from the project has nothing to revert.
        val revert = previouslyApplied
            .filter { it !in assign.keys && it in knownModules }
            .toSet()

        return Plan(assign, revert)
    }
}
