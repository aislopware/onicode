import ExpoModulesCore
import UIKit

enum OniComposerClipboard {
  static let fragmentType = "app.onicode.context-fragment"

  // Pasted thread links become chips in JS; packages/shared/src/threadLinks.ts parses them.
  static func hasThreadLink(_ text: String) -> Bool {
    text.contains("oni-thread://v2/") || text.contains("oni-thread://v3/")
  }

  // JS keeps a rich-text link's destination; packages/shared/src/pastedMarkdownLinks.ts restores it.
  static func hasWebLink(_ html: String) -> Bool {
    html.range(of: #"<a\s[^>]*href"#, options: [.regularExpression, .caseInsensitive]) != nil
  }

  /// Notes and other UIKit apps copy rich text as RTF without HTML; their links reach JS as anchors,
  /// with the RTF's own text for a pasteboard that carries no plain rendition.
  static func rtfLinks(_ board: UIPasteboard) -> (html: String, text: String)? {
    guard let data = board.data(forPasteboardType: "public.rtf"),
          let text = try? NSAttributedString(
            data: data,
            options: [.documentType: NSAttributedString.DocumentType.rtf],
            documentAttributes: nil
          ) else { return nil }
    let escape = { (raw: String) in
      raw.replacingOccurrences(of: "&", with: "&amp;").replacingOccurrences(of: "<", with: "&lt;")
        .replacingOccurrences(of: "\"", with: "&quot;")
    }
    var html = ""
    var linked = false
    text.enumerateAttribute(.link, in: NSRange(location: 0, length: text.length)) { value, range, _ in
      let run = escape(text.attributedSubstring(from: range).string)
      if let href = (value as? URL)?.absoluteString ?? value as? String {
        linked = true
        html += "<a href=\"\(escape(href))\">\(run)</a>"
      } else {
        html += run
      }
    }
    return linked ? (html, text.string) : nil
  }

  static func write(text: String, fragment: String) {
    var items: [String: Any] = ["public.utf8-plain-text": text]
    if let data = fragment.data(using: .utf8),
       var payload = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
       let records = payload["records"] as? [[String: Any]] {
      var selected = records.filter { record in
        guard let id = record["contextId"] as? String else { return false }
        return text.contains("/\(id))")
      }
      let screenshots = Set(selected.compactMap { $0["screenshotContextId"] as? String })
      selected.append(contentsOf: records.filter { screenshots.contains($0["contextId"] as? String ?? "") && !text.contains("/\($0["contextId"] as? String ?? ""))") })
      payload["records"] = selected
      if !selected.isEmpty, let encoded = try? JSONSerialization.data(withJSONObject: payload), let raw = String(data: encoded, encoding: .utf8) {
        let attribute = raw.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? ""
        let escaped = text.replacingOccurrences(of: "&", with: "&amp;").replacingOccurrences(of: "<", with: "&lt;").replacingOccurrences(of: ">", with: "&gt;")
        items[fragmentType] = encoded
        items["public.html"] = Data("<pre data-oni-context-fragment=\"\(attribute)\">\(escaped)</pre>".utf8)
      }
    }
    UIPasteboard.general.items = [items]
  }

  // Keeps the item's other types, such as the HTML and RTF a selection copy carries.
  static func replaceText(_ text: String) {
    let board = UIPasteboard.general
    var item = board.items.first ?? [:]
    item["public.utf8-plain-text"] = text
    board.items = [item]
  }

  static func read() -> [String: String] {
    let board = UIPasteboard.general
    return [
      "text": board.string ?? "",
      "fragment": board.data(forPasteboardType: fragmentType).flatMap { String(data: $0, encoding: .utf8) } ?? "",
      "html": board.data(forPasteboardType: "public.html").flatMap { String(data: $0, encoding: .utf8) } ?? "",
    ]
  }
}

@ExpoModule("OniComposerEditor")
public class OniComposerEditorModule: Module {
  // UIPasteboard is main-thread only, so the member is main-actor isolated.
  @JS
  @MainActor
  func writeContextClipboard(text: String, fragment: String) async {
    OniComposerClipboard.write(text: text, fragment: fragment)
  }

  @JS
  @MainActor
  func replaceClipboardText(text: String) async {
    OniComposerClipboard.replaceText(text)
  }

  public func definition() -> ModuleDefinition {
    View(OniComposerEditorView.self) {
      Prop("controlledDocumentJson") { (view: OniComposerEditorView, documentJson: String) in
        view.setControlledDocumentJson(documentJson)
      }
      Prop("themeJson") { (view: OniComposerEditorView, themeJson: String) in
        view.setThemeJson(themeJson)
      }
      Prop("clipboardFragment") { (view: OniComposerEditorView, fragment: String) in
        view.setClipboardFragment(fragment)
      }
      Prop("placeholder") { (view: OniComposerEditorView, placeholder: String) in
        view.setPlaceholder(placeholder)
      }
      Prop("fontFamily") { (view: OniComposerEditorView, fontFamily: String) in
        view.setFontFamily(fontFamily)
      }
      Prop("fontSize") { (view: OniComposerEditorView, fontSize: Double) in
        view.setFontSize(CGFloat(fontSize))
      }
      Prop("lineHeight") { (view: OniComposerEditorView, lineHeight: Double) in
        view.setLineHeight(CGFloat(lineHeight))
      }
      Prop("contentInsetVertical") { (view: OniComposerEditorView, contentInsetVertical: Double) in
        view.setContentInsetVertical(CGFloat(contentInsetVertical))
      }
      Prop("editable") { (view: OniComposerEditorView, editable: Bool) in
        view.setEditable(editable)
      }
      Prop("readOnly") { (view: OniComposerEditorView, readOnly: Bool) in
        view.setReadOnly(readOnly)
      }
      Prop("scrollEnabled") { (view: OniComposerEditorView, scrollEnabled: Bool) in
        view.setScrollEnabled(scrollEnabled)
      }
      Prop("autoFocus") { (view: OniComposerEditorView, autoFocus: Bool) in
        view.setAutoFocus(autoFocus)
      }
      Prop("autoCorrect") { (view: OniComposerEditorView, autoCorrect: Bool) in
        view.setAutoCorrect(autoCorrect)
      }
      Prop("spellCheck") { (view: OniComposerEditorView, spellCheck: Bool) in
        view.setSpellCheck(spellCheck)
      }
      Prop("submitTitle") { (view: OniComposerEditorView, title: String) in
        view.setSubmitTitle(title)
      }
      Prop("alternateSubmitTitle") { (view: OniComposerEditorView, title: String) in
        view.setAlternateSubmitTitle(title)
      }
      Prop("keyCommandTitles") { (view: OniComposerEditorView, titles: [String: String]) in
        view.setKeyCommandTitles(titles)
      }
      Prop("enterBehavior") { (view: OniComposerEditorView, behavior: String) in
        view.setEnterBehavior(behavior)
      }
      Prop("promptRecallEnabled") { (view: OniComposerEditorView, enabled: Bool) in
        view.setPromptRecallEnabled(enabled)
      }
      Prop("textPasteThresholdBytes") { (view: OniComposerEditorView, threshold: Int) in
        view.setTextPasteThresholdBytes(threshold)
      }
      Prop("maxInputChars") { (view: OniComposerEditorView, maxInputChars: Int) in
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
        "onComposerImageReadStart",
        "onComposerContextPress",
        "onComposerPasteContext",
        "onComposerPasteText",
        "onComposerContentSizeChange"
      )

      AsyncFunction("focus") { (view: OniComposerEditorView) in
        view.focusEditor()
      }
      AsyncFunction("blur") { (view: OniComposerEditorView) in
        view.blurEditor()
      }
      AsyncFunction("setSelection") { (view: OniComposerEditorView, start: Int, end: Int) in
        view.setSelection(start: start, end: end)
      }
      AsyncFunction("readState") { (view: OniComposerEditorView) -> [String: Any] in
        view.readState()
      }
    }
  }
}
