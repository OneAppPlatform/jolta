package dev.jolta.intellij

import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.openapi.roots.ProjectRootManager
import com.intellij.openapi.startup.ProjectActivity

class JoltaStartup : ProjectActivity {
    override suspend fun execute(project: Project) {
        if (Jolta.binary() != null) {
            project.service<JoltaSyncService>().start()
            return
        }
        offerInstallOnce(project)
    }

    /**
     * jolta isn't here. Say so once, in a project where it would actually help
     * — an unprompted "install this" balloon in a project with no Java in it is
     * just an advert.
     */
    private fun offerInstallOnce(project: Project) {
        val state = project.service<JoltaProjectState>()
        if (state.suppressInstallOffer) return
        val dir = project.basePath?.let { java.io.File(it) } ?: return
        val looksLikeJava = ProjectRootManager.getInstance(project).projectSdk != null ||
            Jolta.pinFileExists(dir)
        if (!looksLikeJava) return

        state.suppressInstallOffer = true
        JoltaInstaller.offer(
            project,
            if (Jolta.pinFileExists(dir)) {
                "This project pins a Java version, but the jolta CLI isn't installed."
            } else {
                "The Jolta plugin is installed, but the jolta CLI isn't."
            },
        )
    }
}
