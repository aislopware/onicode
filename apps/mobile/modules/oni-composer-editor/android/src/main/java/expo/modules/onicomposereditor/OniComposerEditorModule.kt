package expo.modules.onicomposereditor

import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import org.json.JSONObject
import org.json.JSONArray

internal object OniComposerClipboard {
  // Pasted thread links become chips in JS; packages/shared/src/threadLinks.ts parses them.
  fun hasThreadLink(text: String): Boolean =
    text.contains("oni-thread://v2/") || text.contains("oni-thread://v3/")

  // JS keeps a rich-text link's destination; packages/shared/src/pastedMarkdownLinks.ts restores it.
  private val webLink = Regex("<a\\s[^>]*href", RegexOption.IGNORE_CASE)

  fun hasWebLink(html: String): Boolean = webLink.containsMatchIn(html)

  // Clipboard bridges can echo our text back without its HTML context metadata.
  private var copiedContext: Pair<String, String>? = null

  @Synchronized
  fun write(context: Context, text: String, fragment: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    val payload = try {
      JSONObject(fragment)
    } catch (_: Exception) {
      null
    }
    val records = payload?.optJSONArray("records")
    if (payload == null || records == null) {
      copiedContext = null
      clipboard.setPrimaryClip(ClipData.newPlainText("Onicode", text))
      return
    }
    val all = (0 until records.length()).map { records.getJSONObject(it) }
    val selected = all.filter { text.contains("/${it.optString("contextId")})") }.toMutableList()
    val screenshots = selected.map { it.optString("screenshotContextId") }.toSet()
    selected.addAll(
      all.filter {
        screenshots.contains(it.optString("contextId")) &&
          !selected.contains(it)
      }
    )
    payload.put("records", JSONArray(selected))
    val serialized = payload.toString()
    val encoded = java.net.URLEncoder.encode(serialized, "UTF-8").replace("+", "%20")
    val escaped = text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
    copiedContext = if (selected.isEmpty()) null else text to serialized
    clipboard.setPrimaryClip(
      if (selected.isEmpty()) {
        ClipData.newPlainText(
          "Onicode",
          text
        )
      } else {
        ClipData.newHtmlText(
          "Onicode",
          text,
          "<pre data-oni-context-fragment=\"$encoded\">$escaped</pre>"
        )
      }
    )
  }

  // Keeps the HTML a selection copy carries.
  @Synchronized
  fun replaceText(context: Context, text: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    val clip = clipboard.primaryClip
    val html = if (clip != null && clip.itemCount > 0) clip.getItemAt(0).htmlText else null
    copiedContext = null
    clipboard.setPrimaryClip(
      if (html == null) {
        ClipData.newPlainText("Onicode", text)
      } else {
        ClipData.newHtmlText("Onicode", text, html)
      }
    )
  }

  @Synchronized
  fun read(context: Context): Map<String, String> {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    val clip = clipboard.primaryClip
    val item = if (clip != null && clip.itemCount > 0) clip.getItemAt(0) else null
    val text = item?.text?.toString() ?: ""
    val html = item?.htmlText ?: ""
    val copied = copiedContext?.takeIf {
      clip?.itemCount == 1 && item?.uri == null && it.first == text
    }
    if (copied == null) copiedContext = null
    return mapOf(
      "text" to text,
      "html" to html,
      "fragment" to
        if (!html.contains("data-oni-context-fragment=")) copied?.second.orEmpty() else ""
    )
  }
}

class OniComposerEditorModule : Module() {
  @Suppress("LongMethod") // A flat list of the view's props and events.
  override fun definition() = ModuleDefinition {
    Name("OniComposerEditor")

    AsyncFunction("writeContextClipboard") { text: String, fragment: String ->
      OniComposerClipboard.write(requireNotNull(appContext.reactContext), text, fragment)
    }

    AsyncFunction("replaceClipboardText") { text: String ->
      OniComposerClipboard.replaceText(requireNotNull(appContext.reactContext), text)
    }

    View(OniComposerEditorView::class) {
      Prop("controlledDocumentJson") { view: OniComposerEditorView, documentJson: String ->
        view.setControlledDocumentJson(documentJson)
      }
      Prop("themeJson") { view: OniComposerEditorView, themeJson: String ->
        view.setThemeJson(themeJson)
      }
      Prop("clipboardFragment") { view: OniComposerEditorView, fragment: String ->
        view.setClipboardFragment(fragment)
      }
      Prop("placeholder") { view: OniComposerEditorView, placeholder: String ->
        view.setPlaceholder(placeholder)
      }
      Prop("fontFamily") { view: OniComposerEditorView, fontFamily: String ->
        view.setFontFamily(fontFamily)
      }
      Prop("fontSize") { view: OniComposerEditorView, fontSize: Double ->
        view.setFontSize(fontSize.toFloat())
      }
      Prop("lineHeight") { view: OniComposerEditorView, lineHeight: Double ->
        view.setLineHeight(lineHeight.toFloat())
      }
      Prop("contentInsetVertical") { view: OniComposerEditorView, contentInsetVertical: Double ->
        view.setContentInsetVertical(contentInsetVertical.toInt())
      }

      Prop("singleLineCentered") { view: OniComposerEditorView, singleLineCentered: Boolean ->
        view.setSingleLineCentered(singleLineCentered)
      }
      Prop("editable") { view: OniComposerEditorView, editable: Boolean ->
        view.setEditable(editable)
      }
      Prop("readOnly") { view: OniComposerEditorView, readOnly: Boolean ->
        view.setReadOnly(readOnly)
      }
      Prop("enterBehavior") { view: OniComposerEditorView, behavior: String ->
        view.setEnterBehavior(behavior)
      }
      Prop("submitEnabled") { view: OniComposerEditorView, enabled: Boolean ->
        view.setSubmitEnabled(enabled)
      }
      Prop("promptRecallEnabled") { view: OniComposerEditorView, enabled: Boolean ->
        view.setPromptRecallEnabled(enabled)
      }
      Prop("scrollEnabled") { view: OniComposerEditorView, scrollEnabled: Boolean ->
        view.setScrollEnabled(scrollEnabled)
      }
      Prop("autoFocus") { view: OniComposerEditorView, autoFocus: Boolean ->
        view.setAutoFocus(autoFocus)
      }
      Prop("autoCorrect") { view: OniComposerEditorView, autoCorrect: Boolean ->
        view.setAutoCorrect(autoCorrect)
      }
      Prop("spellCheck") { view: OniComposerEditorView, spellCheck: Boolean ->
        view.setSpellCheck(spellCheck)
      }
      Prop("textPasteThresholdBytes") { view: OniComposerEditorView, threshold: Int ->
        view.setTextPasteThresholdBytes(threshold)
      }
      Prop("maxInputChars") { view: OniComposerEditorView, maxInputChars: Int ->
        view.setMaxInputChars(maxInputChars)
      }

      Events(
        "onComposerChange",
        "onComposerSelectionChange",
        "onComposerFocus",
        "onComposerBlur",
        "onComposerSubmit",
        "onComposerRecallPrompt",
        "onComposerPasteImages",
        "onComposerContextPress",
        "onComposerPasteContext",
        "onComposerPasteText",
        "onComposerContentSizeChange",
      )

      AsyncFunction("focus") { view: OniComposerEditorView ->
        view.focusEditor()
      }
      AsyncFunction("blur") { view: OniComposerEditorView ->
        view.blurEditor()
      }
      AsyncFunction("setSelection") { view: OniComposerEditorView, start: Int, end: Int ->
        view.setSelection(start, end)
      }
    }
  }
}
