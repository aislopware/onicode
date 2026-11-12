package expo.modules.oniagentnotifications

import android.content.ActivityNotFoundException
import android.content.Intent
import android.os.Build
import android.provider.Settings
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition

class OniAgentNotificationsModule : Module() {
  override fun definition() = ModuleDefinition {
    Name("OniAgentNotifications")

    Function("configure") { deviceId: String, scheme: String, ongoingEnabled: Boolean ->
      appContext.reactContext?.let {
        AgentNotifications.configure(it, deviceId, scheme, ongoingEnabled)
      }
    }

    Function("clear") {
      appContext.reactContext?.let { AgentNotifications.clear(it) }
    }

    Function("clearEnvironment") { environmentId: String ->
      appContext.reactContext?.let { AgentNotifications.clearEnvironment(it, environmentId) }
    }

    Function("setThreadOnScreen") { path: String? ->
      AgentNotifications.setThreadOnScreen(path)
      if (path != null) appContext.reactContext?.let { AgentNotifications.clearThread(it, path) }
    }

    Function("openLiveUpdateSettings") {
      val context = appContext.reactContext
      if (context == null || Build.VERSION.SDK_INT < 36) {
        false
      } else {
        try {
          context.startActivity(
            Intent(Settings.ACTION_APP_NOTIFICATION_PROMOTION_SETTINGS)
              .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
              .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
          )
          true
        } catch (_: ActivityNotFoundException) {
          false
        }
      }
    }
  }
}
