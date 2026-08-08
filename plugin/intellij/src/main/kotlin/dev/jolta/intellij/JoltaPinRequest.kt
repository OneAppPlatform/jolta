package dev.jolta.intellij

/**
 * What the picker is asking for, separated from the Swing that collects it.
 *
 * Validation lives here rather than in the dialog so the rules can be tested
 * without a UI, and so the status bar, the notification actions, and the dialog
 * cannot drift into disagreeing about what a legal pin is.
 */
data class JoltaPinRequest(
    var spec: String = "",
    var exact: Boolean = false,
) {
    /** What actually gets written — punctuation-only noise trimmed off. */
    fun normalized(): String = Jolta.normalizeSpec(spec)

    /** Null when this is safe to write; otherwise the message to show. */
    fun validate(): String? {
        val typed = spec.trim()
        if (typed.isEmpty()) return "Choose a JDK or type a version"
        if (Jolta.isPinnable(typed)) return null

        // An unknown distribution is a different mistake from an unparseable
        // version, and saying which one saves the user a guess.
        val vendorPart = Jolta.normalizeSpec(typed).substringBefore('@', "")
        return if (vendorPart.isNotEmpty() && typed.contains('@')) {
            "jolta doesn't know the distribution '$vendorPart' — try ${KNOWN_VENDOR_HINT}"
        } else {
            "Not a version jolta can pin — try 21, corretto@21, or lts"
        }
    }

    companion object {
        const val KNOWN_VENDOR_HINT =
            "temurin, corretto, zulu, graalvm, liberica, microsoft, semeru, oracle, sapmachine, openjdk"

        /** The request a listed JDK turns into when picked. */
        fun forJdk(jdk: JoltaJdk): JoltaPinRequest = JoltaPinRequest(Jolta.specFor(jdk.vendor, jdk.major))
    }
}
