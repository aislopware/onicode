package expo.modules.oniagentnotifications

import android.app.AlarmManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.net.Uri
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
import com.google.firebase.messaging.RemoteMessage
import expo.modules.notifications.service.ExpoFirebaseMessagingService

class AgentMessagingService : ExpoFirebaseMessagingService() {
  override fun onMessageReceived(remoteMessage: RemoteMessage) {
    if (remoteMessage.data["oni_kind"] == "agent_activity") {
      AgentNotifications.receive(this, remoteMessage.data)
    } else {
      super.onMessageReceived(remoteMessage)
    }
  }
}

class AgentActivityDismissReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    intent.getStringExtra(AgentNotifications.EXTRA_ENVIRONMENT)?.let {
      AgentNotifications.dismiss(context, it)
    }
  }
}

class AgentActivityExpiryReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    intent.getStringExtra(AgentNotifications.EXTRA_ENVIRONMENT)?.let {
      AgentNotifications.expire(context, it)
    }
  }
}

/**
 * Handles gateway data pushes natively so delivery does not depend on a running JS bridge.
 * Each environment pushes its own activity, so each gets its own ongoing card and state.
 */
object AgentNotifications {
  internal const val EXTRA_ENVIRONMENT = "environment_id"
  private const val STORE = "oni-agent-notifications"
  private const val ACTIVITY_CHANNEL = "agent-activity"
  private const val ALERT_CHANNEL = "agent-alerts"
  private const val ACTIVITY_TAG = "oni-agent-activity"
  private const val ALERT_TAG = "oni-agent-alert"
  private const val ENVIRONMENT_PREFIX = "environment:"
  private const val MAX_MESSAGE_AGE_MS = 10 * 60 * 1000L
  private const val RUNNING_LIFETIME_MS = 2 * 60 * 60 * 1000L
  private const val MAX_LIFETIME_MS = 24 * 60 * 60 * 1000L

  private fun store(context: Context) = context.getSharedPreferences(STORE, Context.MODE_PRIVATE)

  private fun key(environmentId: String, name: String) = "$ENVIRONMENT_PREFIX$environmentId|$name"

  internal fun activityId(environmentId: String) = "activity:$environmentId".hashCode()

  @Synchronized
  fun configure(context: Context, deviceId: String, scheme: String, ongoingEnabled: Boolean) {
    val prefs = store(context)
    // A new install id means a new registration; pushes for the old one must not show.
    if (prefs.getString("deviceId", null) != deviceId) clear(context)
    val wasEnabled = prefs.getBoolean("ongoing", false)
    prefs.edit().putString("deviceId", deviceId).putString("scheme", scheme)
      .putBoolean("enabled", true).putBoolean("ongoing", ongoingEnabled).apply()
    if (ongoingEnabled && !wasEnabled) {
      val editor = prefs.edit()
      prefs.all.keys.filter { it.startsWith(ENVIRONMENT_PREFIX) && it.endsWith("|dismissed") }
        .forEach { editor.remove(it) }
      editor.apply()
    }
    if (!ongoingEnabled) environments(prefs).forEach { cancelActivity(context, it) }
    channels(context)
  }

  @Synchronized
  fun clear(context: Context) {
    environments(store(context)).forEach { cancelActivity(context, it) }
    store(context).edit().clear().apply()
    val manager = manager(context)
    manager.activeNotifications.filter {
      it.tag == ACTIVITY_TAG || it.tag == ALERT_TAG ||
        it.tag?.startsWith("$ALERT_TAG-summary:") == true
    }
      .forEach { manager.cancel(it.tag, it.id) }
  }

  @Synchronized
  fun clearEnvironment(context: Context, environmentId: String) {
    cancelActivity(context, environmentId)
    val prefs = store(context)
    val editor = prefs.edit()
    prefs.all.keys.filter { it.startsWith("$ENVIRONMENT_PREFIX$environmentId|") }
      .forEach { editor.remove(it) }
    editor.apply()
  }

  @Volatile private var threadOnScreen: String? = null

  fun setThreadOnScreen(path: String?) {
    threadOnScreen = path
  }

  /** Someone looking at a thread has seen its news, so the alert it left behind goes. */
  @Synchronized
  fun clearThread(context: Context, path: String) {
    val manager = manager(context)
    val id = path.hashCode()
    val alert = manager.activeNotifications.firstOrNull { it.tag == ALERT_TAG && it.id == id }
      ?: return
    val group = alert.notification.group
    manager.cancel(ALERT_TAG, id)
    // A summary over one alert, or none, has nothing left to gather.
    val others = manager.activeNotifications.count {
      it.tag == ALERT_TAG && it.id != id && it.notification.group == group
    }
    if (others < 2) manager.cancel("$ALERT_TAG-summary:$group", 0)
  }

  @Synchronized
  fun dismiss(context: Context, environmentId: String) {
    store(context).edit().putBoolean(key(environmentId, "dismissed"), true).apply()
    cancelActivity(context, environmentId)
  }

  @Synchronized
  fun expire(context: Context, environmentId: String, now: Long = System.currentTimeMillis()) {
    val expiresAt = store(context).getLong(key(environmentId, "expiresAt"), 0)
    // An already-dispatched alarm must not remove a newer run's card.
    if (expiresAt > 0 && expiresAt <= now) cancelActivity(context, environmentId)
  }

  @Synchronized
  fun receive(context: Context, data: Map<String, String>) {
    val prefs = store(context)
    val updatedAt = data["updated_at"]?.toLongOrNull() ?: return
    val registered = prefs.getBoolean("enabled", false) &&
      data["device_id"] == prefs.getString("deviceId", null)
    val fresh = System.currentTimeMillis() - updatedAt in -MAX_MESSAGE_AGE_MS..MAX_MESSAGE_AGE_MS
    if (registered && fresh && NotificationManagerCompat.from(context).areNotificationsEnabled()) {
      channels(context)
      val scheme = prefs.getString("scheme", "onicode") ?: "onicode"
      val alerted = showAlert(context, prefs, scheme, data)
      val environmentId = data["environment_id"]?.takeIf { it.isNotBlank() }
      // Messages without `active` carry no activity change; the card stays as it is.
      if (environmentId != null && data.containsKey("active")) {
        updateActivity(context, environmentId, data, updatedAt, alerted)
      }
    }
  }

  private fun environments(prefs: SharedPreferences): Set<String> =
    prefs.all.keys.filter { it.startsWith(ENVIRONMENT_PREFIX) }
      .map { it.removePrefix(ENVIRONMENT_PREFIX).substringBeforeLast('|') }.toSet()

  private fun showAlert(
    context: Context,
    prefs: SharedPreferences,
    scheme: String,
    data: Map<String, String>
  ): Boolean {
    // Retries carry the same alert id. Keep a bounded history even when
    // notification A is retried after notification B has already arrived.
    val alertId = data["alert_id"]
    val seen = prefs.getString("seenAlertsOrdered", null)?.split('\n').orEmpty()
    if (alertId != null && alertId !in seen) {
      // Consume suppressed alerts so retries cannot resurface them later.
      val resumed = ProcessLifecycleOwner.get().lifecycle.currentState.isAtLeast(
        Lifecycle.State.RESUMED
      )
      val visibleThread = threadOnScreen
      val onScreen = resumed && visibleThread != null && data["alert_path"] == visibleThread
      if (!onScreen) {
        postAlert(context, scheme, data, alertId)
      }
      prefs.edit().putString(
        "seenAlertsOrdered",
        (seen.takeLast(63) + alertId).joinToString("\n")
      ).apply()
      // Android drops posts to a blocked channel, so that alert shows nothing.
      return !onScreen && (
        Build.VERSION.SDK_INT < 26 ||
          manager(context).getNotificationChannel(ALERT_CHANNEL)?.importance !=
          NotificationManager.IMPORTANCE_NONE
        )
    }
    return false
  }

  private fun postAlert(
    context: Context,
    scheme: String,
    data: Map<String, String>,
    alertId: String
  ) {
    val title = data["alert_title"].orEmpty().take(120)
    // Grouped alerts list up to five 120-character thread titles.
    val body = data["alert_body"].orEmpty().take(608)
    // A thread keeps one alert: its newest news replaces what it said before.
    val id = (data["alert_path"]?.takeIf { it.startsWith("/threads/") } ?: alertId).hashCode()
    // One machine's threads gather in one stack.
    val group = "$ALERT_TAG:${data["environment_id"].orEmpty()}"
    val updatedAt = data["updated_at"]?.toLongOrNull() ?: 0L
    val shown = manager(context).activeNotifications.firstOrNull {
      it.tag == ALERT_TAG && it.id == id
    }
    // Pushes can arrive out of order; a late one must not put older news over newer.
    if (shown != null && shown.notification.`when` > updatedAt) return
    val notification = base(context, ALERT_CHANNEL)
      .setWhen(updatedAt)
      .setContentTitle(title).setContentText(body)
      .setStyle(NotificationCompat.BigTextStyle().bigText(body))
      .setAutoCancel(true)
      .setGroup(group)
      .setContentIntent(contentIntent(context, scheme, data["alert_path"], id))
      .build()
    manager(context).notify(ALERT_TAG, id, notification)
    val children = manager(context).activeNotifications.filter {
      it.tag == ALERT_TAG && it.notification.group == group
    }
    if (children.size > 1) {
      val style = NotificationCompat.InboxStyle()
      children.forEach {
        val extras = it.notification.extras
        style.addLine(
          "${extras.getCharSequence(Notification.EXTRA_TITLE)} · " +
            "${extras.getCharSequence(Notification.EXTRA_TEXT)}"
        )
      }
      val summary = base(context, ALERT_CHANNEL)
        .setContentTitle(title)
        .setContentText(body)
        .setStyle(style)
        .setGroup(group)
        .setGroupSummary(true)
        .setSilent(true)
        .setAutoCancel(true)
        .setContentIntent(contentIntent(context, scheme, data["alert_path"], 0))
        .build()
      manager(context).notify("$ALERT_TAG-summary:$group", 0, summary)
    }
  }

  private fun updateActivity(
    context: Context,
    environmentId: String,
    data: Map<String, String>,
    updatedAt: Long,
    alerted: Boolean
  ) {
    val prefs = store(context)
    val k = { name: String -> key(environmentId, name) }
    val terminal = singleTerminal(data)
    // Only a one-row card on the alert's own thread is known to show nothing more
    // than the alert. Grouped alerts and cards with other rows stay visible.
    val covered = terminal?.takeIf { alerted && data["alert_path"] == data["activity_path"] }
    if (covered != null &&
      (
        updatedAt >= prefs.getLong(k("lastUpdate"), 0) ||
          prefs.getString(k("lastTerminal"), null) == covered
        )
    ) {
      prefs.edit().putString(k("coveredTerminal"), covered).apply()
    }
    val isCovered = { value: String? ->
      value != null &&
        value == prefs.getString(k("coveredTerminal"), null)
    }
    if (updatedAt < prefs.getLong(k("lastUpdate"), 0)) {
      // A newer replay of this same finish may already show the card.
      if (isCovered(
          prefs.getString(k("lastTerminal"), null)
        )
      ) {
        cancelActivity(context, environmentId)
      }
      return
    }
    prefs.edit().putLong(k("lastUpdate"), updatedAt).putString(k("lastTerminal"), terminal).apply()
    presentActivity(context, environmentId, data, updatedAt, isCovered(terminal))
  }

  /** Shows, refreshes or removes an environment's card for the newest update it has seen. */
  private fun presentActivity(
    context: Context,
    environmentId: String,
    data: Map<String, String>,
    updatedAt: Long,
    covered: Boolean
  ) {
    val prefs = store(context)
    val k = { name: String -> key(environmentId, name) }
    val active = data["active"] == "true"
    // Absolute expiry: a replay must not extend a finished card or keep an abandoned host active.
    val expiresAt = data["activity_expires_at"]?.toLongOrNull()?.takeIf { it > 0 }
      ?: if (active) updatedAt + RUNNING_LIFETIME_MS else 0L
    val remainingMs = (expiresAt - System.currentTimeMillis()).coerceAtMost(MAX_LIFETIME_MS)
    val wasActive = prefs.getBoolean(k("lastActive"), false)
    prefs.edit().putBoolean(k("lastActive"), active).apply()
    if (remainingMs <= 0 || !prefs.getBoolean("ongoing", false)) {
      cancelActivity(context, environmentId)
      prefs.edit().putBoolean(k("dismissed"), false).apply()
      return
    }
    // Dismissing a run includes its finished card. A new run, or toggling
    // activity off/on, arms it again; terminal replays stay dismissed.
    if (active && !wasActive) prefs.edit().putBoolean(k("dismissed"), false).apply()
    if (prefs.getBoolean(k("dismissed"), false) || covered) {
      cancelActivity(context, environmentId)
    } else {
      showActivity(context, environmentId, data, active, remainingMs)
    }
  }

  /** Identifies a finished one-thread card by its path, phase and absolute expiry. */
  private fun singleTerminal(data: Map<String, String>): String? {
    val phase = data["activity_phase"]
    val finished = phase == "completed" || phase == "failed"
    val single = data.containsKey("activity_line_0") && !data.containsKey("activity_line_1")
    val expiresAt = data["activity_expires_at"]
    return "${data["activity_path"]}\n$phase\n$expiresAt"
      .takeIf { finished && single && expiresAt != null }
  }

  private fun showActivity(
    context: Context,
    environmentId: String,
    data: Map<String, String>,
    active: Boolean,
    remainingMs: Long
  ) {
    val id = activityId(environmentId)
    val scheme = store(context).getString("scheme", "onicode") ?: "onicode"
    val dismissIntent = PendingIntent.getBroadcast(
      context,
      id,
      Intent(context, AgentActivityDismissReceiver::class.java)
        .putExtra(EXTRA_ENVIRONMENT, environmentId),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )
    val presentation = ActivityPresentation(data, active)
    val openThread = contentIntent(context, scheme, data["activity_path"], id)
    val builder = base(context, ACTIVITY_CHANNEL)
      .setOngoing(active).setOnlyAlertOnce(true).setSilent(true)
      .setTimeoutAfter(remainingMs)
      // Live Updates must remain uncolorized to qualify for promotion.
      .setColorized(false)
      .setRequestPromotedOngoing(active)
      .setShortCriticalText(presentation.chip)
      .setContentIntent(openThread)
      .setDeleteIntent(dismissIntent)
    presentation.applyTo(builder, context)
    val action = presentation.action
    if (action != null) {
      if (openThread != null) builder.addAction(0, action, openThread)
      builder.addAction(0, "Dismiss", dismissIntent)
    }
    manager(context).notify(ACTIVITY_TAG, id, builder.build())
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
      // Notification timeouts were added in API 26. One inexact alarm also
      // expires cards on Android 7, including when the app process has exited.
      val expiresAt = System.currentTimeMillis() + remainingMs
      store(context).edit().putLong(key(environmentId, "expiresAt"), expiresAt).apply()
      context.getSystemService(AlarmManager::class.java)
        .setAndAllowWhileIdle(
          AlarmManager.RTC_WAKEUP,
          expiresAt,
          expiryIntent(context, environmentId)
        )
    }
  }

  private fun expiryIntent(context: Context, environmentId: String): PendingIntent =
    PendingIntent.getBroadcast(
      context,
      activityId(environmentId),
      Intent(context, AgentActivityExpiryReceiver::class.java)
        .putExtra(EXTRA_ENVIRONMENT, environmentId),
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )

  private fun cancelActivity(context: Context, environmentId: String) {
    manager(context).cancel(ACTIVITY_TAG, activityId(environmentId))
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
      context.getSystemService(
        AlarmManager::class.java
      ).cancel(expiryIntent(context, environmentId))
      store(context).edit().remove(key(environmentId, "expiresAt")).apply()
    }
  }

  private fun manager(context: Context) = context.getSystemService(NotificationManager::class.java)

  private fun channels(context: Context) {
    if (Build.VERSION.SDK_INT >= 26) {
      manager(context).createNotificationChannels(
        listOf(
          NotificationChannel(ALERT_CHANNEL, "Agent alerts", NotificationManager.IMPORTANCE_HIGH),
          NotificationChannel(
            ACTIVITY_CHANNEL,
            "Ongoing agent activity",
            NotificationManager.IMPORTANCE_LOW
          ),
        )
      )
    }
  }

  private fun base(context: Context, channel: String): NotificationCompat.Builder =
    NotificationCompat.Builder(context, channel)
      .setPriority(
        if (channel ==
          ALERT_CHANNEL
        ) {
          NotificationCompat.PRIORITY_HIGH
        } else {
          NotificationCompat.PRIORITY_LOW
        }
      )
      .setDefaults(if (channel == ALERT_CHANNEL) Notification.DEFAULT_ALL else 0)
      .setSmallIcon(R.drawable.agent_activity_mark)
      .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
      .setShowWhen(false)

  private fun contentIntent(
    context: Context,
    scheme: String,
    path: String?,
    id: Int
  ): PendingIntent? {
    val threadPath = path?.takeIf { it.startsWith("/threads/") }
    val route = threadPath?.takeUnless { it.contains('?') || it.contains('#') } ?: "/"
    val intent = context.packageManager.getLaunchIntentForPackage(context.packageName)
      ?: return null
    intent.setAction(Intent.ACTION_VIEW).setData(Uri.parse("$scheme:/$route"))
      .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    return PendingIntent.getActivity(
      context,
      id,
      intent,
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )
  }
}
