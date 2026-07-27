package dev.jolta.intellij

import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity

class JoltaStartup : ProjectActivity {
    override suspend fun execute(project: Project) {
        project.service<JoltaSyncService>().start()
    }
}
