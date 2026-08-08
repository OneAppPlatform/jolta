package dev.jolta.intellij

/**
 * Whether the IDE's Java configuration is a deliberate departure from the pin.
 *
 * Extracted as a pure decision because it governs when the plugin overwrites a
 * user's choice, and that is not something to leave untested inside a service
 * that needs a live IDE to run.
 *
 * jolta's stamp is global — `$JOLTA_HOME/.stamp`, bumped by `install`, `pin`,
 * `upgrade` and `uninstall` from any directory — so every open project re-syncs
 * whenever anything happens anywhere on the machine. Re-asserting on each of
 * those meant a JDK chosen in Project Structure silently reverted minutes
 * later. So: a manual change stands until the pin itself moves, at which point
 * the pin is new information and wins.
 */
object JoltaOverride {

    /** Identity of a resolved pin. When this moves, the pin gets its way again. */
    fun pinKey(current: JoltaCurrent): String =
        "${current.vendor ?: "jdk"}|${current.version}|${current.home}"

    /**
     * @param appliedSdkName the SDK this plugin last set, or null if it never has
     * @param appliedPinKey  the pin it was set for
     * @param actualSdkName  the project's SDK right now
     */
    fun isManualOverride(
        appliedSdkName: String?,
        appliedPinKey: String?,
        actualSdkName: String?,
        current: JoltaCurrent,
    ): Boolean {
        // Never applied here: whatever is set predates us and is not an override
        // to respect — the first sync is allowed to configure the project.
        val applied = appliedSdkName ?: return false
        // The pin moved since we last applied: new instruction, so it wins.
        if (appliedPinKey != pinKey(current)) return false
        return actualSdkName != applied
    }
}
