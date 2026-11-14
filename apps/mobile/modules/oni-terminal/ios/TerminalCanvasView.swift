import CoreText
import QuartzCore
import UIKit

private enum TerminalFontFiles {
  static let regular = load("MesloLGS-NF-Regular")
  static let bold = load("MesloLGS-NF-Bold")

  // The pod ships the Android app's bundled fonts in its OniTerminalFonts resource bundle.
  private static func load(_ name: String) -> CGFont? {
    let bundle = [Bundle(for: TerminalCanvasView.self), Bundle.main].lazy
      .compactMap { $0.url(forResource: "OniTerminalFonts", withExtension: "bundle") }
      .compactMap(Bundle.init(url:))
      .first
    guard let url = bundle?.url(forResource: name, withExtension: "ttf"),
      let provider = CGDataProvider(url: url as CFURL)
    else { return nil }
    return CGFont(provider)
  }
}

enum TerminalGlyph {
  case single(CTFont, CGGlyph)
  /// Multi-scalar graphemes need shaping; drawn in the context's fill color.
  case line(CTLine)
}

/// Fonts, cell metrics and the glyph cache for one font size.
final class TerminalFontSet {
  let cellWidth: CGFloat
  let cellHeight: CGFloat
  let baseline: CGFloat
  /// Below the baseline.
  let underlineOffset: CGFloat
  /// Above the baseline.
  let strikethroughOffset: CGFloat
  let lineThickness: CGFloat

  private let fonts: [CTFont]
  private var asciiGlyphs: [[TerminalGlyph?]]
  private var glyphs: [[String: TerminalGlyph]]
  private static let glyphCacheLimit = 4096

  init(size: CGFloat, scale: CGFloat) {
    let fallback = CTFontCreateWithName("Menlo-Regular" as CFString, size, nil)
    func make(_ file: CGFont?, italic: Bool) -> CTFont {
      // Meslo has no italic face; slant it the way Android synthesizes one.
      var slant = CGAffineTransform(a: 1, b: 0, c: italic ? 0.2 : 0, d: 1, tx: 0, ty: 0)
      guard let file else { return CTFontCreateCopyWithAttributes(fallback, size, &slant, nil) }
      return CTFontCreateWithGraphicsFont(file, size, &slant, nil)
    }
    let regular = make(TerminalFontFiles.regular, italic: false)
    let bold = make(TerminalFontFiles.bold ?? TerminalFontFiles.regular, italic: false)
    fonts = [
      regular,
      bold,
      make(TerminalFontFiles.regular, italic: true),
      make(TerminalFontFiles.bold ?? TerminalFontFiles.regular, italic: true),
    ]
    asciiGlyphs = Array(repeating: Array(repeating: nil, count: 128), count: 4)
    glyphs = Array(repeating: [:], count: 4)

    func pixels(_ value: CGFloat) -> CGFloat { ceil(value * scale) / scale }
    var character: UniChar = 0x4D  // "M"
    var glyph: CGGlyph = 0
    CTFontGetGlyphsForCharacters(regular, &character, &glyph, 1)
    var advance = CGSize.zero
    CTFontGetAdvancesForGlyphs(regular, .horizontal, &glyph, &advance, 1)
    let ascent = CTFontGetAscent(regular)
    let glyphHeight = ascent + CTFontGetDescent(regular)
    cellWidth = max(pixels(advance.width), 1 / scale)
    cellHeight = max(pixels(glyphHeight * 1.12), 1 / scale)
    baseline = (((cellHeight - glyphHeight) / 2 + ascent) * scale).rounded() / scale
    lineThickness = max(CTFontGetUnderlineThickness(regular), 1 / scale)
    underlineOffset = max(-CTFontGetUnderlinePosition(regular), lineThickness)
    strikethroughOffset = CTFontGetXHeight(regular) / 2
  }

  func glyph(for text: String, flags: UInt16) -> TerminalGlyph? {
    let variant =
      (flags & TerminalFrame.flagBold != 0 ? 1 : 0) | (flags & TerminalFrame.flagItalic != 0 ? 2 : 0)
    let scalars = text.unicodeScalars
    if scalars.count == 1, let scalar = scalars.first, scalar.value < 0x80 {
      if let cached = asciiGlyphs[variant][Int(scalar.value)] { return cached }
      let resolved = resolve(text, font: fonts[variant])
      asciiGlyphs[variant][Int(scalar.value)] = resolved
      return resolved
    }
    if let cached = glyphs[variant][text] { return cached }
    guard let resolved = resolve(text, font: fonts[variant]) else { return nil }
    if glyphs[variant].count >= Self.glyphCacheLimit { glyphs[variant].removeAll() }
    glyphs[variant][text] = resolved
    return resolved
  }

  private func resolve(_ text: String, font: CTFont) -> TerminalGlyph? {
    let units = Array(text.utf16)
    guard !units.isEmpty else { return nil }
    if text.unicodeScalars.count == 1 {
      var glyphs = [CGGlyph](repeating: 0, count: units.count)
      if CTFontGetGlyphsForCharacters(font, units, &glyphs, units.count), glyphs[0] != 0 {
        return .single(font, glyphs[0])
      }
      let fallback = CTFontCreateForString(font, text as CFString, CFRange(location: 0, length: units.count))
      if CTFontGetGlyphsForCharacters(fallback, units, &glyphs, units.count), glyphs[0] != 0 {
        return .single(fallback, glyphs[0])
      }
    }
    let attributes: [NSAttributedString.Key: Any] = [
      NSAttributedString.Key(kCTFontAttributeName as String): font,
      NSAttributedString.Key(kCTForegroundColorFromContextAttributeName as String): true,
    ]
    return .line(CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: attributes)))
  }
}

private extension CGContext {
  func setFill(argb: UInt32) {
    setFillColor(
      red: CGFloat((argb >> 16) & 0xFF) / 255,
      green: CGFloat((argb >> 8) & 0xFF) / 255,
      blue: CGFloat(argb & 0xFF) / 255,
      alpha: 1)
  }

  func setStroke(argb: UInt32) {
    setStrokeColor(
      red: CGFloat((argb >> 16) & 0xFF) / 255,
      green: CGFloat((argb >> 8) & 0xFF) / 255,
      blue: CGFloat(argb & 0xFF) / 255,
      alpha: 1)
  }

  /// Flips to CoreText's y-up space for a view of `height`; draw text at `height - baseline`.
  func beginText(height: CGFloat) {
    saveGState()
    translateBy(x: 0, y: height)
    scaleBy(x: 1, y: -1)
    textMatrix = .identity
  }

  func draw(_ glyph: TerminalGlyph, at point: CGPoint) {
    switch glyph {
    case let .single(font, cgGlyph):
      var glyphs = [cgGlyph]
      var position = point
      textPosition = .zero
      CTFontDrawGlyphs(font, &glyphs, &position, 1, self)
    case let .line(line):
      textPosition = point
      CTLineDraw(line, self)
    }
  }
}

func terminalBlend(_ foreground: UInt32, _ background: UInt32, amount: CGFloat) -> UInt32 {
  func channel(_ shift: UInt32) -> UInt32 {
    let front = CGFloat((foreground >> shift) & 0xFF)
    let back = CGFloat((background >> shift) & 0xFF)
    return UInt32(front * amount + back * (1 - amount)) << shift
  }
  return 0xFF00_0000 | channel(16) | channel(8) | channel(0)
}

/// Draws `TerminalFrame`s and turns swipes into scroll rows; a port of Android's TerminalCanvasView.kt.
final class TerminalCanvasView: UIView {
  static let contentPadding: CGFloat = 8
  private static let selectionAmount: CGFloat = 0.32
  private static let decelerationPerMillisecond: CGFloat = 0.998
  private static let minimumFlingVelocity: CGFloat = 20

  private(set) var fonts: TerminalFontSet
  private(set) var frameData: TerminalFrame?
  var cursorTextColor: UInt32?
  /// Rows to scroll (positive toward newer output) and the viewport cell under the touch.
  var onScrollRows: ((Int, Int, Int) -> Void)?

  private let cursorView = TerminalCursorView()
  private let scrollGesture = UIPanGestureRecognizer()
  private var blinkTimer: Timer?
  private var scrollRemainder: CGFloat = 0
  private var flingLink: CADisplayLink?
  private var flingVelocity: CGFloat = 0
  private var flingLocation = CGPoint.zero
  private var flingTimestamp: CFTimeInterval = 0
  private var drawnSize = CGSize.zero
  private var fontScale: CGFloat

  var fontSize: CGFloat {
    didSet {
      guard oldValue != fontSize else { return }
      updateFonts()
    }
  }

  init(fontSize: CGFloat) {
    self.fontSize = fontSize
    fontScale = max(UITraitCollection.current.displayScale, 1)
    fonts = TerminalFontSet(size: fontSize, scale: fontScale)
    super.init(frame: .zero)
    contentScaleFactor = fontScale
    isOpaque = true
    contentMode = .topLeft
    cursorView.isUserInteractionEnabled = false
    cursorView.isHidden = true
    addSubview(cursorView)
    scrollGesture.addTarget(self, action: #selector(handleScroll(_:)))
    scrollGesture.maximumNumberOfTouches = 1
    scrollGesture.cancelsTouchesInView = false
    addGestureRecognizer(scrollGesture)
  }

  required init?(coder: NSCoder) {
    return nil
  }

  deinit {
    blinkTimer?.invalidate()
    flingLink?.invalidate()
  }

  var usableSize: CGSize {
    CGSize(
      width: max(bounds.width - Self.contentPadding * 2, 1),
      height: max(bounds.height - Self.contentPadding * 2, 1))
  }

  /// Returns whether the cell metrics changed.
  @discardableResult
  func updateScale(_ scale: CGFloat) -> Bool {
    guard scale > 0, scale != fontScale else { return false }
    fontScale = scale
    contentScaleFactor = scale
    cursorView.contentScaleFactor = scale
    updateFonts()
    return true
  }

  func setFrame(_ frame: TerminalFrame) {
    frameData = frame
    setNeedsDisplay()
    updateCursor()
  }

  func cell(at point: CGPoint) -> (col: Int, row: Int) {
    let cols = frameData?.cols ?? 1
    let rows = frameData?.rows ?? 1
    let col = Int((point.x - Self.contentPadding) / fonts.cellWidth)
    let row = Int((point.y - Self.contentPadding) / fonts.cellHeight)
    return (min(max(col, 0), max(cols - 1, 0)), min(max(row, 0), max(rows - 1, 0)))
  }

  func stopFling() {
    flingLink?.invalidate()
    flingLink = nil
  }

  override func didMoveToWindow() {
    super.didMoveToWindow()
    if window == nil {
      stopFling()
      blinkTimer?.invalidate()
      blinkTimer = nil
    } else {
      updateCursor()
    }
  }

  override func layoutSubviews() {
    super.layoutSubviews()
    // contentMode .topLeft keeps old pixels in place; repaint for the new size.
    if bounds.size != drawnSize {
      drawnSize = bounds.size
      setNeedsDisplay()
    }
  }

  private func updateFonts() {
    fonts = TerminalFontSet(size: fontSize, scale: fontScale)
    setNeedsDisplay()
    updateCursor()
  }

  // MARK: Drawing

  override func draw(_ rect: CGRect) {
    guard let context = UIGraphicsGetCurrentContext() else { return }
    guard let frame = frameData else {
      context.setFillColor((backgroundColor ?? .black).cgColor)
      context.fill(rect)
      return
    }
    let padding = Self.contentPadding
    let cellHeight = fonts.cellHeight
    context.setFill(argb: frame.background)
    context.fill(bounds)
    context.saveGState()
    context.clip(
      to: CGRect(
        x: padding, y: padding, width: bounds.width - padding * 2, height: bounds.height - padding * 2))

    let firstRow = max(Int((rect.minY - padding) / cellHeight), 0)
    let lastRow = min(Int(ceil((rect.maxY - padding) / cellHeight)), frame.rows)
    guard firstRow < lastRow else {
      context.restoreGState()
      return
    }

    let rows = firstRow..<lastRow
    drawBackgrounds(frame, in: context, rows: rows)
    let decorations = drawText(frame, in: context, rows: rows)

    for (rect, color) in decorations {
      context.setFill(argb: color)
      context.fill(rect)
    }
    context.restoreGState()
  }

  private func drawBackgrounds(_ frame: TerminalFrame, in context: CGContext, rows: Range<Int>) {
    let padding = Self.contentPadding
    let cellWidth = fonts.cellWidth
    let cellHeight = fonts.cellHeight
    // Backgrounds, one fill per run of equal colour.
    for row in rows {
      let top = padding + CGFloat(row) * cellHeight
      var runStart = 0
      var runColor: UInt32 = 0
      for column in 0...frame.cols {
        var color = frame.background
        if column < frame.cols {
          let index = row * frame.cols + column
          color = frame.cellBackgrounds[index]
          if frame.cellFlags[index] & TerminalFrame.flagSelected != 0 {
            color = terminalBlend(frame.cursorColor, color, amount: Self.selectionAmount)
          }
        }
        if column > 0, color == runColor, column < frame.cols { continue }
        if column > 0, runColor != frame.background {
          context.setFill(argb: runColor)
          context.fill(
            CGRect(
              x: padding + CGFloat(runStart) * cellWidth, y: top,
              width: CGFloat(column - runStart) * cellWidth, height: cellHeight))
        }
        runStart = column
        runColor = color
      }
    }
  }

  private func drawText(_ frame: TerminalFrame, in context: CGContext, rows: Range<Int>) -> [(CGRect, UInt32)] {
    let padding = Self.contentPadding
    let cellWidth = fonts.cellWidth
    let cellHeight = fonts.cellHeight
    // Text, batching single glyphs that share a font and colour.
    var decorations: [(CGRect, UInt32)] = []
    let height = bounds.height
    context.beginText(height: height)
    var batchFont: CTFont?
    var batchColor: UInt32 = 0
    var batchGlyphs: [CGGlyph] = []
    var batchPositions: [CGPoint] = []
    func flush() {
      guard let font = batchFont, !batchGlyphs.isEmpty else { return }
      context.setFill(argb: batchColor)
      context.textPosition = .zero
      CTFontDrawGlyphs(font, batchGlyphs, batchPositions, batchGlyphs.count, context)
      batchGlyphs.removeAll(keepingCapacity: true)
      batchPositions.removeAll(keepingCapacity: true)
    }
    for row in rows {
      let top = padding + CGFloat(row) * cellHeight
      let baselineY = height - (top + fonts.baseline)
      for column in 0..<frame.cols {
        let index = row * frame.cols + column
        let text = frame.cellText[index]
        let flags = frame.cellFlags[index]
        guard !text.isEmpty, flags & TerminalFrame.flagInvisible == 0,
          let glyph = fonts.glyph(for: text, flags: flags)
        else { continue }
        let color = frame.cellForegrounds[index]
        let left = padding + CGFloat(column) * cellWidth
        switch glyph {
        case let .single(font, cgGlyph):
          if batchFont !== font || batchColor != color {
            flush()
            batchFont = font
            batchColor = color
          }
          batchGlyphs.append(cgGlyph)
          batchPositions.append(CGPoint(x: left, y: baselineY))
        case .line:
          flush()
          context.setFill(argb: color)
          context.draw(glyph, at: CGPoint(x: left, y: baselineY))
        }
        appendDecorations(flags: flags, color: color, left: left, top: top, to: &decorations)
      }
    }
    flush()
    context.restoreGState()
    return decorations
  }

  private func appendDecorations(
    flags: UInt16, color: UInt32, left: CGFloat, top: CGFloat, to decorations: inout [(CGRect, UInt32)]
  ) {
    let width = fonts.cellWidth
    let thickness = fonts.lineThickness
    let baseline = top + fonts.baseline
    if flags & TerminalFrame.flagUnderline != 0 {
      decorations.append(
        (CGRect(x: left, y: baseline + fonts.underlineOffset - thickness / 2, width: width, height: thickness), color))
    }
    if flags & TerminalFrame.flagStrikethrough != 0 {
      decorations.append(
        (CGRect(x: left, y: baseline - fonts.strikethroughOffset - thickness / 2, width: width, height: thickness), color))
    }
    if flags & TerminalFrame.flagOverline != 0 {
      decorations.append((CGRect(x: left, y: top, width: width, height: max(thickness, 1)), color))
    }
  }

  // MARK: Cursor

  private func updateCursor() {
    blinkTimer?.invalidate()
    blinkTimer = nil
    guard let frame = frameData, frame.cursorVisible,
      frame.cursorX < frame.cols, frame.cursorY < frame.rows
    else {
      cursorView.isHidden = true
      return
    }
    let index = frame.cursorY * frame.cols + frame.cursorX
    cursorView.configure(
      TerminalCursorView.Appearance(
        style: frame.cursorStyle,
        color: frame.cursorColor,
        text: frame.cellText[index],
        flags: frame.cellFlags[index],
        textColor: cursorTextColor ?? frame.background),
      fonts: fonts)
    cursorView.frame = CGRect(
      x: Self.contentPadding + CGFloat(frame.cursorX) * fonts.cellWidth,
      y: Self.contentPadding + CGFloat(frame.cursorY) * fonts.cellHeight,
      width: fonts.cellWidth,
      height: fonts.cellHeight)
    cursorView.isHidden = false
    // Blinking toggles only the cursor's own layer, never the grid.
    guard frame.cursorBlinking, window != nil else { return }
    blinkTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
      guard let self else { return }
      self.cursorView.isHidden.toggle()
    }
  }

  // MARK: Scrolling

  @objc
  private func handleScroll(_ gesture: UIPanGestureRecognizer) {
    switch gesture.state {
    case .began:
      stopFling()
      scrollRemainder = 0
    case .changed:
      scroll(by: gesture.translation(in: self).y, at: gesture.location(in: self))
    case .ended:
      let velocity = gesture.velocity(in: self).y
      if abs(velocity) > Self.minimumFlingVelocity * 10 {
        startFling(velocity: velocity, at: gesture.location(in: self))
      }
    default:
      break
    }
    gesture.setTranslation(.zero, in: self)
  }

  /// Dragging down by one cell height reveals one older row.
  private func scroll(by points: CGFloat, at location: CGPoint) {
    scrollRemainder -= points / fonts.cellHeight
    let rows = Int(scrollRemainder)
    guard rows != 0 else { return }
    scrollRemainder -= CGFloat(rows)
    let cell = cell(at: location)
    onScrollRows?(rows, cell.col, cell.row)
  }

  private func startFling(velocity: CGFloat, at location: CGPoint) {
    stopFling()
    flingVelocity = velocity
    flingLocation = location
    flingTimestamp = 0
    let link = CADisplayLink(target: TerminalWeakTarget(self), selector: #selector(TerminalWeakTarget.tick(_:)))
    link.add(to: .main, forMode: .common)
    flingLink = link
  }

  fileprivate func flingTick(_ link: CADisplayLink) {
    if flingTimestamp == 0 {
      flingTimestamp = link.timestamp
      return
    }
    let milliseconds = CGFloat((link.timestamp - flingTimestamp) * 1000)
    flingTimestamp = link.timestamp
    let decay = pow(Self.decelerationPerMillisecond, milliseconds)
    let distance = flingVelocity * (decay - 1) / log(Self.decelerationPerMillisecond) / 1000
    flingVelocity *= decay
    scroll(by: distance, at: flingLocation)
    if abs(flingVelocity) < Self.minimumFlingVelocity { stopFling() }
  }
}

/// CADisplayLink retains its target; this keeps it from retaining the canvas.
private final class TerminalWeakTarget: NSObject {
  weak var canvas: TerminalCanvasView?

  init(_ canvas: TerminalCanvasView) {
    self.canvas = canvas
    super.init()
  }

  @objc
  func tick(_ link: CADisplayLink) {
    guard let canvas else {
      link.invalidate()
      return
    }
    canvas.flingTick(link)
  }
}

private final class TerminalCursorView: UIView {
  struct Appearance: Equatable {
    var style = 1
    var color: UInt32 = 0
    var text = ""
    var flags: UInt16 = 0
    var textColor: UInt32 = 0
  }

  private var appearance = Appearance()
  private var fonts: TerminalFontSet?

  override init(frame: CGRect) {
    super.init(frame: frame)
    isOpaque = false
    backgroundColor = .clear
    contentMode = .redraw
  }

  required init?(coder: NSCoder) {
    return nil
  }

  func configure(_ appearance: Appearance, fonts: TerminalFontSet) {
    guard appearance != self.appearance || fonts !== self.fonts else { return }
    self.appearance = appearance
    self.fonts = fonts
    setNeedsDisplay()
  }

  // Styles follow GhosttyCursorStyle: 0 bar, 1 block, 2 underline, 3 hollow block.
  override func draw(_ rect: CGRect) {
    guard let context = UIGraphicsGetCurrentContext(), let fonts else { return }
    let style = appearance.style
    let color = appearance.color
    let text = appearance.text
    context.setFill(argb: color)
    switch style {
    case 0:
      context.fill(CGRect(x: 0, y: 0, width: 2, height: bounds.height))
    case 2:
      context.fill(CGRect(x: 0, y: bounds.height - 2, width: bounds.width, height: 2))
    case 3:
      context.setStroke(argb: color)
      context.setLineWidth(1)
      context.stroke(bounds.insetBy(dx: 0.5, dy: 0.5))
    default:
      context.fill(bounds)
      guard !text.isEmpty, let glyph = fonts.glyph(for: text, flags: appearance.flags) else { return }
      context.beginText(height: bounds.height)
      context.setFill(argb: appearance.textColor)
      context.draw(glyph, at: CGPoint(x: 0, y: bounds.height - fonts.baseline))
      context.restoreGState()
    }
  }
}
