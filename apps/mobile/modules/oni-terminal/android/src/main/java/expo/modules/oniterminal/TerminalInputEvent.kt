package expo.modules.oniterminal

import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction

/**
 * Terminal replies as an `onInput` event: UTF-8 text, or one character per byte with `binary`
 * when the bytes are not UTF-8, as legacy X10 mouse reports past column 95 are not.
 */
internal fun terminalInputEvent(bytes: ByteArray): Map<String, Any> {
  val decoder = Charsets.UTF_8.newDecoder()
    .onMalformedInput(CodingErrorAction.REPORT)
    .onUnmappableCharacter(CodingErrorAction.REPORT)
  return try {
    mapOf("data" to decoder.decode(ByteBuffer.wrap(bytes)).toString())
  } catch (_: CharacterCodingException) {
    mapOf("data" to String(bytes, Charsets.ISO_8859_1), "binary" to true)
  }
}
