package expo.modules.oniagentnotifications

import android.content.Context
import android.icu.text.MessageFormat
import java.util.Locale

/**
 * Copy of the notifications, with the English an older JS bundle gets. `key` names the app's
 * translation in `androidNotificationStrings.ts`; counted texts are ICU patterns.
 */
internal enum class AgentText(val key: String, val english: String) {
  ALERT_CHANNEL("alertChannel", "Agent alerts"),
  ACTIVITY_CHANNEL("activityChannel", "Ongoing agent activity"),

  ACTIVITY("activity", "Agent activity"),
  ALL_FINISHED("allFinished", "All finished"),
  NEED_YOU("needYou", "{count, plural, one {{count} needs you} other {{count} need you}}"),
  FAILED_COUNT("failedCount", "{count, plural, one {{count} failed} other {{count} failed}}"),
  FINISHED_FAILED_COUNT(
    "finishedFailedCount",
    "{count, plural, one {Finished, {count} failed} other {Finished, {count} failed}}"
  ),
  DONE_COUNT("doneCount", "{count, plural, one {{count} done} other {{count} done}}"),
  WORKING_COUNT("workingCount", "{count, plural, one {{count} working} other {{count} working}}"),
  ACTIVE_COUNT("activeCount", "{count, plural, one {{count} active} other {{count} active}}"),
  THREAD_COUNT("threadCount", "{count, plural, one {{count} thread} other {{count} threads}}"),

  STATUS_CONNECTING("statusConnecting", "Connecting"),
  STATUS_WORKING("statusWorking", "Working"),
  STATUS_APPROVAL("statusApproval", "Approval"),
  STATUS_INPUT("statusInput", "Input"),
  STATUS_WAITING("statusWaiting", "Waiting"),
  STATUS_DONE("statusDone", "Done"),
  STATUS_FAILED("statusFailed", "Failed"),

  CHIP_ACTIVE("chipActive", "Active"),
  CHIP_LIVE("chipLive", "{count} live"),
  CHIP_WORKING("chipWorking", "Working"),
  CHIP_APPROVE("chipApprove", "Approve"),
  CHIP_ANSWER("chipAnswer", "Answer"),
  CHIP_WAITING("chipWaiting", "Waiting"),
  CHIP_DONE("chipDone", "Done"),
  CHIP_FAILED("chipFailed", "Failed"),

  ACTION_OPEN("actionOpen", "Open"),
  ACTION_APPROVE("actionApprove", "Approve"),
  ACTION_ANSWER("actionAnswer", "Answer"),
  ACTION_DISMISS("actionDismiss", "Dismiss")
}

/** The app's language, kept on disk because pushes also arrive while JS is not running. */
internal class AgentNotificationText(private val translations: Map<String, String>) {
  private val locale = Locale.forLanguageTag(translations[LOCALE_KEY] ?: "en")

  operator fun get(text: AgentText): String = translated(text) ?: text.english

  fun format(text: AgentText, count: Any): String = try {
    translated(text)?.let { pattern(it, locale, count) }
  } catch (_: IllegalArgumentException) {
    null
  } ?: pattern(text.english, Locale.ENGLISH, count)

  private fun translated(text: AgentText) = translations[text.key]?.takeIf { it.isNotBlank() }

  private fun pattern(pattern: String, locale: Locale, count: Any) =
    MessageFormat(pattern, locale).format(mapOf("count" to count))

  companion object {
    // Apart from the registration's store, which a new install id or opting out clears.
    private const val STORE = "oni-agent-notification-text"
    private const val LOCALE_KEY = "locale"

    private fun store(context: Context) = context.getSharedPreferences(STORE, Context.MODE_PRIVATE)

    fun load(context: Context) = AgentNotificationText(
      store(context).all.mapNotNull { (key, value) -> (value as? String)?.let { key to it } }
        .toMap()
    )

    fun save(context: Context, translations: Map<String, String>) {
      val editor = store(context).edit().clear()
      translations.forEach { (key, value) -> editor.putString(key, value) }
      editor.apply()
    }
  }
}
