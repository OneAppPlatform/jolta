import org.jetbrains.intellij.platform.gradle.TestFrameworkType

plugins {
    id("java")
    id("org.jetbrains.kotlin.jvm") version "2.0.21"
    id("org.jetbrains.intellij.platform") version "2.2.1"
}

group = "dev.jolta"
version = "0.2.0"

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
    pluginVerification {
        ides {
            // sinceBuild floor and a current release: the API drift that
            // breaks plugins shows up at the ends of the supported range
            ide("IC", "2024.2.4")
            ide("IC", "2025.1.4")
        }
    }
}

// Platform tests drive the IDE's own component container; they need the
// module system opened up and won't run headless without this.
tasks.test {
    systemProperty("java.awt.headless", "true")
    jvmArgs(
        "--add-opens=java.base/java.lang=ALL-UNNAMED",
        "--add-opens=java.base/java.util=ALL-UNNAMED",
    )
}
