package dev.jolta.intellij

import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.project.Project

/** Everything the plugin says to the user goes through the one "Jolta" group. */
object JoltaNotifications {
    fun notify(
        project: Project,
        text: String,
        type: NotificationType,
        vararg actions: NotificationAction,
    ) {
        val notification = NotificationGroupManager.getInstance()
            .getNotificationGroup("Jolta")
            .createNotification(text, type)
        actions.forEach(notification::addAction)
        notification.notify(project)
    }
}
