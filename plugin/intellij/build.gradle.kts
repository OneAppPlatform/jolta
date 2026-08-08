import org.jetbrains.intellij.platform.gradle.TestFrameworkType

plugins {
    id("java")
    id("org.jetbrains.kotlin.jvm") version "2.0.21"
    id("org.jetbrains.intellij.platform") version "2.2.1"
}

group = "dev.jolta"
version = "1.0.0"

repositories {
    mavenCentral()
    intellijPlatform {
        defaultRepositories()
    }
}

dependencies {
    intellijPlatform {
        intellijIdeaCommunity("2024.2.4")
        bundledPlugin("com.intellij.java")
        bundledPlugin("com.intellij.gradle")
        bundledPlugin("org.jetbrains.idea.maven")
        bundledPlugin("org.jetbrains.plugins.terminal")
        testFramework(TestFrameworkType.Platform)
    }
    testImplementation("junit:junit:4.13.2")
}

kotlin {
    // 2024.2 runs on 21; building at 17 produces class files the platform
    // verifier rejects
    jvmToolchain(21)
}

intellijPlatform {
    buildSearchableOptions = false // no settings UI to index
    pluginConfiguration {
        ideaVersion {
            sinceBuild = "242"
            untilBuild = provider { null }
        }
    }
    // Marketplace signing. Unsigned plugins install with a warning dialog, so
    // this is effectively required for anything published. Credentials come
    // from the environment and are never committed; when they're absent the
    // tasks simply don't run, so local builds are unaffected.
    // Paths, not contents: key material stays on disk instead of in the
    // environment (where it shows up in process listings), and the verify task
    // only accepts a file anyway.
    signing {
        certificateChainFile = layout.file(
            providers.environmentVariable("JB_CERTIFICATE_CHAIN_FILE").map { file(it) },
        )
        privateKeyFile = layout.file(
            providers.environmentVariable("JB_PRIVATE_KEY_FILE").map { file(it) },
        )
        password = providers.environmentVariable("JB_PRIVATE_KEY_PASSWORD")
    }

    publishing {
        token = providers.environmentVariable("JB_PUBLISH_TOKEN")
        // Anything with a pre-release suffix goes to a side channel rather than
        // to everyone on stable.
        channels = providers.gradleProperty("pluginVersion").orElse(project.version.toString())
            .map { v -> listOf(v.substringAfter('-', "default").substringBefore('.')) }
    }

    pluginVerification {
        ides {
            // sinceBuild floor and a current release: the API drift that
            // breaks plugins shows up at the ends of the supported range
            ide("IC", "2024.2.4")
            ide("IC", "2025.1.4")
        }
    }
}

// The IntelliJ Platform plugin (2.2.1) doesn't wire these together, so running
// both in one invocation fails Gradle's input validation: verification reads
// the archive signing produces.
tasks.named("verifyPluginSignature") { dependsOn(tasks.named("signPlugin")) }

// Platform tests drive the IDE's own component container; they need the
// module system opened up and won't run headless without this.
tasks.test {
    systemProperty("java.awt.headless", "true")
    jvmArgs(
        "--add-opens=java.base/java.lang=ALL-UNNAMED",
        "--add-opens=java.base/java.util=ALL-UNNAMED",
    )
}
