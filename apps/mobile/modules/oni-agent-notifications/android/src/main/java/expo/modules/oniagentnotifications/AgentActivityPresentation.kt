package expo.modules.oniagentnotifications

import android.content.Context
import android.graphics.Typeface
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.StyleSpan
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat

internal data class ActivityRow(val status: String, val title: String, val project: String)

internal enum class ActivityPhase(
  // The word the environment sends in each row, always English.
  val status: String,
  val label: AgentText,
  val chip: AgentText,
  val action: AgentText,
  val color: Int
) {
  STARTING(
    "Connecting",
    AgentText.STATUS_CONNECTING,
    AgentText.CHIP_WORKING,
    AgentText.ACTION_OPEN,
    R.color.agent_activity_working
  ),
  RUNNING(
    "Working",
    AgentText.STATUS_WORKING,
    AgentText.CHIP_WORKING,
    AgentText.ACTION_OPEN,
    R.color.agent_activity_working
  ),
  APPROVAL(
    "Approval",
    AgentText.STATUS_APPROVAL,
    AgentText.CHIP_APPROVE,
    AgentText.ACTION_APPROVE,
    R.color.agent_activity_attention
  ),
  INPUT(
    "Input",
    AgentText.STATUS_INPUT,
    AgentText.CHIP_ANSWER,
    AgentText.ACTION_ANSWER,
    R.color.agent_activity_input
  ),
  STALE(
    "Waiting",
    AgentText.STATUS_WAITING,
    AgentText.CHIP_WAITING,
    AgentText.ACTION_OPEN,
    R.color.agent_activity_waiting
  ),
  COMPLETED(
    "Done",
    AgentText.STATUS_DONE,
    AgentText.CHIP_DONE,
    AgentText.ACTION_OPEN,
    R.color.agent_activity_done
  ),
  FAILED(
    "Failed",
    AgentText.STATUS_FAILED,
    AgentText.CHIP_FAILED,
    AgentText.ACTION_OPEN,
    R.color.agent_activity_failed
  );

  val needsUser get() = this == APPROVAL || this == INPUT
  val finished get() = this == COMPLETED || this == FAILED

  companion object {
    fun forStatus(status: String) = entries.firstOrNull { it.status == status }
  }
}

/** The environment orders rows and their deep link together; never reorder them in the client. */
internal fun activityRows(data: Map<String, String>) = (0..4).mapNotNull {
  val parts = data["activity_line_$it"]?.split('\t', limit = 3) ?: return@mapNotNull null
  if (parts.size != 3 || parts[1].isBlank()) {
    null
  } else {
    ActivityRow(parts[0].take(40), parts[1].take(120), parts[2].take(120))
  }
}

internal fun activityPhase(data: Map<String, String>, rows: List<ActivityRow>): ActivityPhase? =
  when (data["activity_phase"]?.takeIf { it.isNotBlank() }) {
    "starting" -> ActivityPhase.STARTING
    "running" -> ActivityPhase.RUNNING
    "waiting_for_approval" -> ActivityPhase.APPROVAL
    "waiting_for_input" -> ActivityPhase.INPUT
    "stale" -> ActivityPhase.STALE
    "completed" -> ActivityPhase.COMPLETED
    "failed" -> ActivityPhase.FAILED
    else -> rows.firstOrNull()?.let { ActivityPhase.forStatus(it.status) }
  }

internal class ActivityPresentation(
  data: Map<String, String>,
  private val active: Boolean,
  private val text: AgentNotificationText
) {
  private val rows = activityRows(data)
  private val hero = rows.firstOrNull()
  val phase = activityPhase(data, rows)
  private val activeCount = data["activity_active_count"]?.toIntOrNull()?.coerceAtLeast(0)
    ?: rows.count { ActivityPhase.forStatus(it.status)?.finished != true }
  private val attentionCount = data["activity_attention_count"]?.toIntOrNull()?.coerceAtLeast(0)
    ?: rows.count { ActivityPhase.forStatus(it.status)?.needsUser == true }
  private val failedCount = data["activity_failed_count"]?.toIntOrNull()?.coerceAtLeast(0)
    ?: rows.count { it.status == ActivityPhase.FAILED.status }
  private val completedCount = data["activity_done_count"]?.toIntOrNull()?.coerceAtLeast(0)
    ?: rows.count { it.status == ActivityPhase.COMPLETED.status }
  val threadCount = activeCount + failedCount + completedCount
  private val singleProject = rows.map { it.project }.distinct().size == 1
  private val legacyBody = (0..4).mapNotNull { data["activity_line_$it"]?.take(300) }
    .takeIf { it.isNotEmpty() }?.joinToString("\n")
    ?: data["activity_body"].orEmpty().take(240)

  val summary = when {
    hero == null -> data["activity_title"]?.takeIf { it.isNotBlank() }?.take(120)
      ?: text[AgentText.ACTIVITY]
    rows.size == 1 -> hero.title
    attentionCount > 0 -> text.format(AgentText.NEED_YOU, attentionCount)
    activeCount > 0 && failedCount > 0 -> text.format(AgentText.FAILED_COUNT, failedCount)
    // The environment leads with a result while others still work; the summary says which.
    activeCount > 0 && phase == ActivityPhase.COMPLETED ->
      text.format(AgentText.DONE_COUNT, completedCount)
    activeCount > 0 -> text.format(AgentText.WORKING_COUNT, activeCount)
    failedCount > 0 -> text.format(AgentText.FINISHED_FAILED_COUNT, failedCount)
    else -> text[AgentText.ALL_FINISHED]
  }

  val chip = when {
    !active -> null
    phase == null -> data["activity_chip"]?.takeIf { it.isNotBlank() }?.take(7)
      ?: text[AgentText.CHIP_ACTIVE]
    phase == ActivityPhase.RUNNING && activeCount > 1 ->
      text.format(AgentText.CHIP_LIVE, if (activeCount > 9) "9+" else activeCount)
    else -> text[phase.chip]
  }

  val action = if (active) text[phase?.action ?: AgentText.ACTION_OPEN] else null

  fun applyTo(builder: NotificationCompat.Builder, context: Context) {
    val tint = phase?.let { ContextCompat.getColor(context, it.color) }
    if (tint != null) builder.setColor(tint)
    // Tint the summary only when it names an outcome or a request; a plain
    // "3 working" stays neutral so the accent keeps meaning something.
    val tintedSummary = tint != null && rows.size > 1 && phase != ActivityPhase.RUNNING &&
      phase != ActivityPhase.STARTING
    builder.setContentTitle(if (tintedSummary) tinted(summary, tint!!) else summary)
    if (rows.size > 1) {
      builder.setSubText(
        listOfNotNull(
          hero!!.project.takeIf { singleProject && it.isNotBlank() },
          if (activeCount > 0) {
            text.format(AgentText.ACTIVE_COUNT, activeCount)
          } else {
            text.format(AgentText.THREAD_COUNT, threadCount)
          }
        ).joinToString(" · ")
      )
    }
    val body = body(context)
    val lineBreak = body.indexOf('\n')
    val firstLine = if (lineBreak >= 0) body.subSequence(0, lineBreak) else body
    builder.setContentText(firstLine).setStyle(NotificationCompat.BigTextStyle().bigText(body))
    // Thread update timestamps can change while an approval remains pending.
    // Leave the timer hidden until the payload has a stable phase-entry timestamp.
    builder.setShowWhen(false).setUsesChronometer(false)
  }

  private fun body(context: Context): CharSequence = when {
    hero == null -> legacyBody
    rows.size == 1 -> statusLine(context, hero.copy(title = hero.project), "")
    else -> SpannableStringBuilder().apply {
      rows.forEachIndexed { index, row ->
        if (index > 0) append("\n")
        append(statusLine(context, row, row.project.takeUnless { singleProject }.orEmpty()))
      }
    }
  }

  private fun statusLine(context: Context, row: ActivityRow, trailing: String): CharSequence =
    SpannableStringBuilder().apply {
      val status = ActivityPhase.forStatus(row.status)
      val color = ContextCompat.getColor(context, status?.color ?: R.color.agent_activity_waiting)
      val label = status?.let { text[it.label] } ?: row.status
      append(tinted(label, color, bold = true))
      append(" ").append(row.title)
      // Promoted cards drop text color, so the separator has to do the work of the dimming.
      if (trailing.isNotBlank()) {
        val start = length
        append(" · ").append(trailing)
        setSpan(
          ForegroundColorSpan(ContextCompat.getColor(context, R.color.agent_activity_waiting)),
          start,
          length,
          Spanned.SPAN_EXCLUSIVE_EXCLUSIVE
        )
      }
    }

  private fun tinted(text: String, color: Int, bold: Boolean = false) =
    SpannableStringBuilder(text).apply {
      setSpan(ForegroundColorSpan(color), 0, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
      if (bold) setSpan(StyleSpan(Typeface.BOLD), 0, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
    }
}
