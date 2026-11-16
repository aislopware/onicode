package expo.modules.oniterminal

import org.junit.Assert.assertEquals
import org.junit.Test

class TerminalInputEventTest {
  @Test
  fun mouseReportPastColumn95KeepsEveryByte() {
    // X10 wheel-up at column 200, row 1: ESC [ M, then 32 + button, 32 + column, 32 + row.
    val report =
      byteArrayOf(0x1b, '['.code.toByte(), 'M'.code.toByte(), 96, (32 + 200).toByte(), 33)
    val event = terminalInputEvent(report)
    assertEquals(true, event["binary"])
    assertEquals(report.toList(), (event["data"] as String).map { it.code.toByte() })
  }

  @Test
  fun utf8RepliesStayText() {
    assertEquals(
      mapOf("data" to "\u001b]0;café\u0007"),
      terminalInputEvent("\u001b]0;café\u0007".toByteArray())
    )
  }
}
