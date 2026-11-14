import ExpoModulesCore
import Foundation
import UIKit

/// While a text field is first responder, iPadOS may consume presses before pressesBegan.
/// Priority UIKeyCommands capture terminal shortcuts before the text-input system handles them.
private enum TerminalHardwareKeyEncoder {
  private static let controlInputs = "abcdefghijklmnopqrstuvwxyz@[\\]^_-? "

  static func makeKeyCommands(action: Selector) -> [UIKeyCommand] {
    var commands: [UIKeyCommand] = []

    let specialInputs = [
      UIKeyCommand.inputEscape,
      UIKeyCommand.inputUpArrow,
      UIKeyCommand.inputDownArrow,
      UIKeyCommand.inputLeftArrow,
      UIKeyCommand.inputRightArrow,
      "\t",
    ]
    for input in specialInputs {
      commands.append(makeCommand(input: input, modifierFlags: [], action: action))
    }
    commands.append(makeCommand(input: "\t", modifierFlags: .shift, action: action))

    for character in controlInputs {
      commands.append(makeCommand(input: String(character), modifierFlags: .control, action: action))
      commands.append(
        makeCommand(input: String(character), modifierFlags: [.control, .shift], action: action)
      )
    }

    return commands
  }

  private static func makeCommand(
    input: String,
    modifierFlags: UIKeyModifierFlags,
    action: Selector
  ) -> UIKeyCommand {
    let command = UIKeyCommand(input: input, modifierFlags: modifierFlags, action: action)
    command.wantsPriorityOverSystemBehavior = true
    return command
  }

  static func sequence(input: String, modifiers: UIKeyModifierFlags) -> String? {
    switch input {
    case UIKeyCommand.inputEscape:
      return "\u{1B}"
    case UIKeyCommand.inputUpArrow:
      return "\u{1B}[A"
    case UIKeyCommand.inputDownArrow:
      return "\u{1B}[B"
    case UIKeyCommand.inputRightArrow:
      return "\u{1B}[C"
    case UIKeyCommand.inputLeftArrow:
      return "\u{1B}[D"
    case "\t":
      return modifiers.contains(.shift) ? "\u{1B}[Z" : "\t"
    default:
      break
    }

    guard modifiers.contains(.control) else { return nil }
    guard let scalar = input.lowercased().unicodeScalars.first else { return nil }
    return controlSequence(for: scalar)
  }

  private static func controlSequence(for scalar: Unicode.Scalar) -> String? {
    switch scalar {
    case "a"..."z":
      return UnicodeScalar(scalar.value - 96).map(String.init)
    case " ", "@":
      return "\u{00}"
    case "[":
      return "\u{1B}"
    case "\\":
      return "\u{1C}"
    case "]":
      return "\u{1D}"
    case "^":
      return "\u{1E}"
    case "_", "-":
      return "\u{1F}"
    case "?":
      return "\u{7F}"
    default:
      return nil
    }
  }
}

private enum TerminalInputSequence {
  /// Terminal Enter is carriage return. Sending line feed instead is Ctrl+J,
  /// which raw-mode TUIs may interpret as the literal J key.
  static let carriageReturn = "\r"

  static func normalizingReturn(_ input: String) -> String {
    switch input {
    case "\n", "\r\n":
      return carriageReturn
    default:
      return input
    }
  }
}

private final class TerminalInputField: UITextField {
  var onDeleteBackward: (() -> Void)?
  var onInsert: ((String) -> Void)?
  var onPaste: (() -> Void)?

  private static let hardwareKeyCommands = TerminalHardwareKeyEncoder.makeKeyCommands(
    action: #selector(handleHardwareKeyCommand(_:))
  )

  // While an IME composes, arrows, Tab and Escape pick or cancel candidates instead.
  override var keyCommands: [UIKeyCommand]? {
    markedTextRange == nil ? Self.hardwareKeyCommands : nil
  }

  override func deleteBackward() {
    // Backspace edits marked or committed field text first; the field diff sends that DEL.
    if markedTextRange == nil, !hasText {
      onDeleteBackward?()
    }
    super.deleteBackward()
  }

  // System pastes must bypass typed input to preserve paste encoding and chunking.
  override func paste(_ sender: Any?) {
    // Commit an in-progress composition first so it reaches the PTY ahead of the paste.
    if markedTextRange != nil {
      unmarkText()
      sendActions(for: .editingChanged)
    }
    onPaste?()
  }

  @objc
  private func handleHardwareKeyCommand(_ command: UIKeyCommand) {
    guard let input = command.input else { return }
    guard let sequence = TerminalHardwareKeyEncoder.sequence(
      input: input,
      modifiers: command.modifierFlags
    ) else { return }
    onInsert?(sequence)
  }
}

private extension UInt32 {
  /// Parses `#RRGGBB` or `#AARRGGBB` into the core's opaque ARGB.
  init?(terminalHex value: String) {
    var hex = Substring(value.trimmingCharacters(in: .whitespaces))
    if hex.hasPrefix("#") { hex = hex.dropFirst() }
    guard hex.count == 6 || hex.count == 8, let parsed = UInt32(hex, radix: 16) else { return nil }
    self = 0xFF00_0000 | (parsed & 0xFF_FFFF)
  }
}

private extension UIColor {
  convenience init(argb: UInt32) {
    self.init(
      red: CGFloat((argb >> 16) & 0xFF) / 255,
      green: CGFloat((argb >> 8) & 0xFF) / 255,
      blue: CGFloat(argb & 0xFF) / 255,
      alpha: 1)
  }
}

private extension String {
  init(terminalBytes bytes: OniTerminalBytes) {
    guard let data = bytes.data else {
      self = ""
      return
    }
    self = String(bytes: UnsafeBufferPointer(start: data, count: bytes.len), encoding: .utf8) ?? ""
  }
}

/// The libghostty-vt terminal surface (via cpp/oni_terminal_core.h); mirrors Android's OniTerminalView.kt.
public final class OniTerminalView: ExpoView, UITextFieldDelegate, UIEditMenuInteractionDelegate {
  private let canvas = TerminalCanvasView(fontSize: 10)
  private let inputField = TerminalInputField()
  private let focusTapGesture = UITapGestureRecognizer()
  private let editMenuGesture = UILongPressGestureRecognizer()
  private lazy var editMenuInteraction = UIEditMenuInteraction(delegate: self)
  private var terminal: OpaquePointer?
  private var fedBuffer = ""
  private var streamedOutput: TerminalOutputBuffer?
  private var grid = (cols: 0, rows: 0)
  private var cellPixels = (width: 0, height: 0)
  private var measuredGrid = (cols: 0, rows: 0)
  private var renderScheduled = false
  private var menuText: String?
  private var sentInput = ""
  private var foregroundColorValue: UInt32 = 0xFFD1_D5DA
  private var backgroundColorValue: UInt32 = 0xFF24_292E
  private var cursorColorValue: UInt32 = 0xFF00_9FFF
  private var cursorTextColorValue: UInt32?
  private var paletteColors: [UInt32] = []

  let onInput = EventDispatcher()
  let onResize = EventDispatcher()
  let onCapture = EventDispatcher()
  let onPaste = EventDispatcher()
  var captureRequest: Double = 0 {
    didSet {
      guard captureRequest > 0, captureRequest != oldValue else { return }
      onCapture(["text": captureText()])
    }
  }

  var terminalKey: String = "" {
    didSet {
      accessibilityIdentifier = "oni-terminal-\(terminalKey)"
      if oldValue != terminalKey {
        recreateTerminal()
      }
    }
  }

  // Expo sets a batch's props in no fixed order, so the buffer waits for the whole batch:
  // output fed before the same batch's sharedGrid lands would be drawn, and clipped, at the
  // previous grid.
  var initialBuffer: String = "" {
    didSet {
      guard oldValue != initialBuffer else { return }
      hasPendingBuffer = true
    }
  }
  private var hasPendingBuffer = false

  // The PTY grid shared by every attached client, or empty before the server reports one.
  // The terminal draws this grid, clamped to the view's own fit (the server's grid lags a
  // local resize, and read-only observers never constrain it), so its wrapping and cursor
  // match the PTY; the view's own fit still goes out through onResize.
  var sharedGrid: [Int] = [] {
    didSet {
      guard oldValue != sharedGrid else { return }
      applyGrid()
    }
  }

  var fontSize: CGFloat = 10 {
    didSet {
      guard oldValue != fontSize else { return }
      inputField.font = UIFont.monospacedSystemFont(ofSize: max(fontSize, 13), weight: .regular)
      canvas.fontSize = fontSize
      emitResize()
    }
  }

  var focusRequest: Double = 0 {
    didSet {
      guard oldValue != focusRequest else { return }
      DispatchQueue.main.async { [weak self] in
        self?.requestKeyboardFocus()
      }
    }
  }

  var readOnly = false {
    didSet {
      if readOnly {
        inputField.resignFirstResponder()
      }
    }
  }

  var autoFocus = true {
    didSet {
      guard oldValue != autoFocus else { return }
      if autoFocus {
        requestKeyboardFocus()
      } else {
        inputField.resignFirstResponder()
      }
    }
  }

  // The core derives the reported color scheme from the background, as on Android.
  var appearanceScheme: String = "dark"

  var themeConfig: String = "" {
    didSet {
      guard oldValue != themeConfig else { return }
      parseThemeConfig(themeConfig)
      applyTheme()
    }
  }

  var backgroundColorHex: String = "#24292e" {
    didSet {
      backgroundColorValue = UInt32(terminalHex: backgroundColorHex) ?? backgroundColorValue
      applyTheme()
    }
  }

  var foregroundColorHex: String = "#d1d5da" {
    didSet {
      foregroundColorValue = UInt32(terminalHex: foregroundColorHex) ?? foregroundColorValue
      applyTheme()
    }
  }

  var mutedForegroundColorHex: String = "#959da5"

  public required init(appContext: AppContext? = nil) {
    super.init(appContext: appContext)
    clipsToBounds = true

    canvas.onScrollRows = { [weak self] rows, col, row in
      self?.scrollRows(rows, col: col, row: row)
    }

    inputField.delegate = self
    inputField.backgroundColor = UIColor.clear
    inputField.textColor = UIColor.clear
    inputField.tintColor = UIColor.clear
    inputField.font = UIFont.monospacedSystemFont(ofSize: max(fontSize, 13), weight: .regular)
    inputField.placeholder = ""
    inputField.autocorrectionType = .no
    inputField.autocapitalizationType = .none
    inputField.spellCheckingType = .no
    inputField.smartDashesType = .no
    inputField.smartQuotesType = .no
    inputField.smartInsertDeleteType = .no
    inputField.returnKeyType = .send
    inputField.keyboardType = .default
    inputField.enablesReturnKeyAutomatically = false
    inputField.translatesAutoresizingMaskIntoConstraints = false
    inputField.alpha = 0.02
    inputField.isAccessibilityElement = false
    inputField.accessibilityElementsHidden = true
    inputField.addTarget(self, action: #selector(handleInputEditingDidBegin), for: .editingDidBegin)
    inputField.addTarget(self, action: #selector(handleInputEditingChanged), for: .editingChanged)
    inputField.onDeleteBackward = { [weak self] in
      self?.emitInput("\u{7F}")
    }
    inputField.onInsert = { [weak self] data in
      self?.emitInput(data)
      self?.resetInputField()
    }
    inputField.onPaste = { [weak self] in
      self?.emitPaste()
    }

    focusTapGesture.addTarget(self, action: #selector(handleViewportTap))
    canvas.addGestureRecognizer(focusTapGesture)
    editMenuGesture.addTarget(self, action: #selector(handleViewportLongPress(_:)))
    canvas.addGestureRecognizer(editMenuGesture)
    canvas.addInteraction(editMenuInteraction)
    focusTapGesture.require(toFail: editMenuGesture)

    addSubview(canvas)
    addSubview(inputField)

    NSLayoutConstraint.activate([
      inputField.trailingAnchor.constraint(equalTo: trailingAnchor),
      inputField.topAnchor.constraint(equalTo: bottomAnchor, constant: 8),
      inputField.widthAnchor.constraint(equalToConstant: 1),
      inputField.heightAnchor.constraint(equalToConstant: 1),
    ])
    applyTheme()
  }

  deinit {
    if let terminal {
      oni_terminal_destroy(terminal)
    }
  }

  public override func layoutSubviews() {
    super.layoutSubviews()
    canvas.frame = bounds
    // The trait collection follows the scene the view is in, which matters once
    // iPhone apps run resizable and on external displays; UIScreen.main does not.
    canvas.updateScale(traitCollection.displayScale)
    emitResize()
  }

  public override func didMoveToWindow() {
    super.didMoveToWindow()

    guard window != nil, autoFocus else { return }
    DispatchQueue.main.async { [weak self] in
      self?.requestKeyboardFocus()
    }
  }

  public func textFieldShouldReturn(_ textField: UITextField) -> Bool {
    // Return during a composition commits it; that is not Enter.
    guard textField.markedTextRange == nil else { return false }
    emitInput(TerminalInputSequence.carriageReturn)
    resetInputField()
    return false
  }

  // MARK: Terminal lifecycle

  private func emitResize() {
    guard bounds.width > 0, bounds.height > 0 else { return }
    let usable = canvas.usableSize
    let cols = min(max(Int(usable.width / canvas.fonts.cellWidth), 2), 400)
    let rows = min(max(Int(usable.height / canvas.fonts.cellHeight), 2), 200)
    if cols != measuredGrid.cols || rows != measuredGrid.rows {
      measuredGrid = (cols, rows)
      onResize(["cols": cols, "rows": rows])
    }
    applyGrid()
  }

  private func applyGrid() {
    guard measuredGrid.cols > 0, measuredGrid.rows > 0 else { return }
    let shared = sharedGrid.count == 2 && sharedGrid[0] > 0 && sharedGrid[1] > 0 ? sharedGrid : nil
    let nextGrid = (
      cols: min(shared?[0] ?? measuredGrid.cols, measuredGrid.cols),
      rows: min(shared?[1] ?? measuredGrid.rows, measuredGrid.rows)
    )
    let scale = canvas.contentScaleFactor
    let nextCell = (
      width: Int((canvas.fonts.cellWidth * scale).rounded()),
      height: Int((canvas.fonts.cellHeight * scale).rounded())
    )
    if nextGrid == grid, nextCell == cellPixels, terminal != nil { return }
    grid = nextGrid
    cellPixels = nextCell
    if let terminal {
      emitResponse(
        oni_terminal_resize(
          terminal, Int32(grid.cols), Int32(grid.rows), Int32(cellPixels.width), Int32(cellPixels.height)))
    } else {
      createTerminal()
    }
    if !hasPendingBuffer { feedPendingBuffer() }
    scheduleRender()
  }

  private func createTerminal() {
    guard terminal == nil, grid.cols > 0, grid.rows > 0 else { return }
    terminal = withTheme { theme in
      oni_terminal_create(
        Int32(grid.cols), Int32(grid.rows), Int32(cellPixels.width), Int32(cellPixels.height), theme)
    }
    fedBuffer = ""
    if let streamedOutput {
      feed(streamedOutput.data)
    }
  }

  private func recreateTerminal() {
    guard terminal != nil else { return }
    destroyTerminal()
    createTerminal()
    if !hasPendingBuffer { feedPendingBuffer() }
    scheduleRender()
  }

  private func destroyTerminal() {
    guard let terminal else { return }
    oni_terminal_destroy(terminal)
    self.terminal = nil
    fedBuffer = ""
  }

  func applyPendingBuffer() {
    guard hasPendingBuffer else { return }
    hasPendingBuffer = false
    feedPendingBuffer()
  }

  private func feedPendingBuffer() {
    guard streamedOutput == nil, terminal != nil, initialBuffer != fedBuffer else { return }
    if !initialBuffer.utf8.starts(with: fedBuffer.utf8) {
      recreateTerminal()
      guard terminal != nil else { return }
    }
    let suffix = initialBuffer.utf8.dropFirst(fedBuffer.utf8.count)
    if !suffix.isEmpty {
      feed(Data(suffix))
    }
    fedBuffer = initialBuffer
    scheduleRender()
  }

  func writeOutput(_ data: String, reset: Bool) {
    let output = streamedOutput ?? TerminalOutputBuffer()
    let bytes = Data(data.utf8)
    output.write(bytes, reset: reset)
    streamedOutput = output
    if reset {
      recreateTerminal()
    } else if terminal != nil {
      feed(bytes)
      scheduleRender()
    }
  }

  private func feed(_ data: Data) {
    guard let terminal, !data.isEmpty else { return }
    let response = data.withUnsafeBytes { buffer in
      oni_terminal_feed(terminal, buffer.bindMemory(to: UInt8.self).baseAddress, buffer.count)
    }
    emitResponse(response)
  }

  private func scrollRows(_ rows: Int, col: Int, row: Int) {
    guard let terminal else { return }
    emitResponse(oni_terminal_scroll_wheel(terminal, Int32(rows), Int32(col), Int32(row)))
    scheduleRender()
  }

  /// Output arrives in bursts; one snapshot per main-queue turn is enough.
  private func scheduleRender() {
    guard !renderScheduled else { return }
    renderScheduled = true
    DispatchQueue.main.async { [weak self] in
      guard let self else { return }
      self.renderScheduled = false
      if let frame = self.snapshot() {
        self.canvas.setFrame(frame)
      }
    }
  }

  private func snapshot() -> TerminalFrame? {
    guard let terminal else { return nil }
    let bytes = oni_terminal_snapshot(terminal)
    defer { oni_terminal_bytes_free(bytes) }
    guard let data = bytes.data else { return nil }
    return TerminalFrame.decode(UnsafeRawBufferPointer(start: data, count: bytes.len))
  }

  private func emitResponse(_ bytes: OniTerminalBytes) {
    defer { oni_terminal_bytes_free(bytes) }
    guard bytes.len > 0 else { return }
    emitInput(String(terminalBytes: bytes))
  }

  private func captureText() -> String {
    guard let frame = snapshot() else { return "" }
    // Android joins snapshot rows with "\n" and trims each row's trailing whitespace; identical
    // terminal content must capture identically on both platforms.
    return (0..<frame.rows).map { row -> String in
      var line = frame.cellText[(row * frame.cols)..<((row + 1) * frame.cols)].joined()
      // Kotlin's trimEnd only strips ASCII whitespace; match it exactly.
      while let last = line.last, last.isASCII && last.isWhitespace { line.removeLast() }
      return line
    }.joined(separator: "\n")
  }

  private func viewportText() -> String? {
    guard let terminal else { return nil }
    var bytes = OniTerminalBytes(data: nil, len: 0)
    guard oni_terminal_viewport_text(terminal, &bytes) else { return nil }
    defer { oni_terminal_bytes_free(bytes) }
    return String(terminalBytes: bytes)
  }

  // MARK: Theme

  private func withTheme<Result>(_ body: (UnsafePointer<OniTerminalTheme>) -> Result) -> Result {
    paletteColors.withUnsafeBufferPointer { palette in
      var theme = OniTerminalTheme(
        foreground: foregroundColorValue,
        background: backgroundColorValue,
        cursor: cursorColorValue,
        palette: palette.baseAddress,
        palette_len: palette.count)
      return withUnsafePointer(to: &theme, body)
    }
  }

  private func applyTheme() {
    let background = UIColor(argb: backgroundColorValue)
    backgroundColor = background
    canvas.backgroundColor = background
    canvas.cursorTextColor = cursorTextColorValue
    guard let terminal else { return }
    emitResponse(withTheme { oni_terminal_set_theme(terminal, $0) })
    scheduleRender()
  }

  private func parseThemeConfig(_ config: String) {
    var palette: [Int: UInt32] = [:]
    for line in config.split(whereSeparator: \.isNewline) {
      let parts = line.split(separator: "=", maxSplits: 1)
      guard parts.count == 2 else { continue }
      let value = parts[1].trimmingCharacters(in: .whitespaces)
      switch parts[0].trimmingCharacters(in: .whitespaces) {
      case "background":
        backgroundColorValue = UInt32(terminalHex: value) ?? backgroundColorValue
      case "foreground":
        foregroundColorValue = UInt32(terminalHex: value) ?? foregroundColorValue
      case "cursor-color":
        cursorColorValue = UInt32(terminalHex: value) ?? cursorColorValue
      case "cursor-text":
        cursorTextColorValue = UInt32(terminalHex: value) ?? cursorTextColorValue
      case "palette":
        let entry = value.split(separator: "=", maxSplits: 1)
        guard entry.count == 2, let index = Int(entry[0].trimmingCharacters(in: .whitespaces)),
          (0...255).contains(index)
        else { continue }
        palette[index] = UInt32(terminalHex: String(entry[1])) ?? foregroundColorValue
      default:
        break
      }
    }
    if let lastIndex = palette.keys.max() {
      paletteColors = (0...lastIndex).map { palette[$0] ?? foregroundColorValue }
    }
  }

  // MARK: Gestures and edit menu

  @objc
  private func handleViewportTap() {
    canvas.stopFling()
    requestKeyboardFocus()
  }

  @objc
  private func handleViewportLongPress(_ gesture: UILongPressGestureRecognizer) {
    guard gesture.state == .began else { return }
    // Freeze the text the user pressed on; output keeps streaming while the menu is open.
    menuText = viewportText().flatMap { $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : $0 }
    guard menuText != nil || canPaste else { return }
    editMenuInteraction.presentEditMenu(
      with: UIEditMenuConfiguration(identifier: nil, sourcePoint: gesture.location(in: canvas))
    )
  }

  public func editMenuInteraction(
    _ interaction: UIEditMenuInteraction,
    menuFor configuration: UIEditMenuConfiguration,
    suggestedActions: [UIMenuElement]
  ) -> UIMenu? {
    // The hidden input field would otherwise add actions such as Scan Text or Writing Tools.
    // Keep the system Paste command: unlike a custom action, it reads without a paste prompt.
    var children: [UIMenuElement] = Self.pasteCommand(in: suggestedActions).map { [$0] } ?? []
    if let text = menuText {
      children.append(
        UIAction(title: "Select Text", image: UIImage(systemName: "text.cursor")) { [weak self] _ in
          self?.presentTextSelection(text)
        })
    }
    return children.isEmpty ? nil : UIMenu(children: children)
  }

  private func presentTextSelection(_ text: String) {
    guard let window else { return }
    var responder: UIResponder? = self
    while let current = responder, !(current is UIViewController) {
      responder = current.next
    }
    guard let presenter = responder as? UIViewController,
      presenter.viewIfLoaded?.window === window,
      presenter.presentedViewController == nil,
      !presenter.isBeingPresented,
      !presenter.isBeingDismissed,
      presenter.transitionCoordinator == nil
    else { return }

    inputField.resignFirstResponder()
    let controller = TerminalTextSelectionViewController(text: text, fontSize: fontSize)
    let navigation = UINavigationController(rootViewController: controller)
    navigation.modalPresentationStyle = .pageSheet
    presenter.present(navigation, animated: !UIAccessibility.isReduceMotionEnabled)
  }

  private static func pasteCommand(in elements: [UIMenuElement]) -> UIMenuElement? {
    for element in elements {
      if let command = element as? UICommand,
         command.action == #selector(UIResponderStandardEditActions.paste(_:)) {
        return command
      }
      if let menu = element as? UIMenu, let command = pasteCommand(in: menu.children) {
        return command
      }
    }
    return nil
  }

  public override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
    if action == #selector(UIResponderStandardEditActions.paste(_:)) {
      return canPaste
    }
    return super.canPerformAction(action, withSender: sender)
  }

  public override func paste(_ sender: Any?) {
    emitPaste()
  }

  private var canPaste: Bool {
    !readOnly && UIPasteboard.general.hasStrings
  }

  // MARK: Input

  // Korean keyboards build a syllable by replacing the committed character before the cursor,
  // so the field keeps what was sent and each change goes out as DELs plus the new suffix.
  // Scalars, not Characters: a Thai or Indic mark joins the previous grapheme, and resending
  // that grapheme after one DEL would leave its base twice in a code-point line editor.
  @objc
  private func handleInputEditingChanged() {
    guard inputField.markedTextRange == nil else { return }
    let text = inputField.text ?? ""
    let fieldScalars = text.unicodeScalars
    let sentScalars = sentInput.unicodeScalars
    let commonCount = zip(fieldScalars, sentScalars).prefix { $0.0 == $0.1 }.count
    let unsent = String(String.UnicodeScalarView(sentScalars.dropFirst(commonCount)))
    let added = String(String.UnicodeScalarView(fieldScalars.dropFirst(commonCount)))
    // Some software keyboards deliver Return as an inserted newline instead of textFieldShouldReturn.
    emitInput(String(repeating: "\u{7F}", count: unsent.count) + TerminalInputSequence.normalizingReturn(added))
    sentInput = text
    // A Hangul syllable never spans whitespace; past the cap, keep the last character to compose onto.
    if text.last?.isWhitespace == true {
      resetInputField()
    } else if text.count > 64 {
      let last = String(text.suffix(1))
      inputField.text = last
      sentInput = last
    }
  }

  @objc
  private func handleInputEditingDidBegin() {
    // A composition abandoned by losing focus must not reach the PTY with the next keystroke.
    resetInputField()
  }

  func resetInputField() {
    inputField.text = ""
    sentInput = ""
  }

  private func requestKeyboardFocus() {
    guard window != nil, !readOnly else { return }
    inputField.becomeFirstResponder()
  }

  private func emitInput(_ data: String) {
    guard !readOnly, !data.isEmpty else { return }
    onInput(["data": data])
  }

  private func emitPaste() {
    guard let text = UIPasteboard.general.string, !text.isEmpty else { return }
    onPaste(["text": text])
  }
}
