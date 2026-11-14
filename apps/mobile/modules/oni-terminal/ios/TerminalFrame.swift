import Foundation

/// A decoded `oni_terminal_snapshot` (layout in cpp/oni_terminal_core.h); mirrors Android's TerminalFrame.kt.
struct TerminalFrame {
  static let flagBold: UInt16 = 1 << 0
  static let flagItalic: UInt16 = 1 << 1
  static let flagInvisible: UInt16 = 1 << 4
  static let flagStrikethrough: UInt16 = 1 << 5
  static let flagOverline: UInt16 = 1 << 6
  static let flagUnderline: UInt16 = 1 << 7
  static let flagSelected: UInt16 = 1 << 8

  let cols: Int
  let rows: Int
  let cursorX: Int
  let cursorY: Int
  let cursorVisible: Bool
  let cursorStyle: Int
  let cursorBlinking: Bool
  let foreground: UInt32
  let background: UInt32
  let cursorColor: UInt32
  let cellForegrounds: [UInt32]
  let cellBackgrounds: [UInt32]
  let cellFlags: [UInt16]
  let cellText: [String]

  private static let magic: UInt32 = 0x5456_4E4F
  private static let version: UInt16 = 1
  private static let headerBytes = 32
  private static let cellHeaderBytes = 12
  private static let asciiText: [String] = (0..<128).map { String(UnicodeScalar(UInt8($0))) }

  static func decode(_ bytes: UnsafeRawBufferPointer) -> TerminalFrame? {
    guard bytes.count >= headerBytes else { return nil }
    var offset = 0
    func u8() -> UInt8 {
      defer { offset += 1 }
      return bytes[offset]
    }
    func u16() -> UInt16 {
      defer { offset += 2 }
      return UInt16(littleEndian: bytes.loadUnaligned(fromByteOffset: offset, as: UInt16.self))
    }
    func u32() -> UInt32 {
      defer { offset += 4 }
      return UInt32(littleEndian: bytes.loadUnaligned(fromByteOffset: offset, as: UInt32.self))
    }

    guard u32() == magic, u16() == version else { return nil }
    let cols = Int(u16())
    let rows = Int(u16())
    let cursorX = Int(u16())
    let cursorY = Int(u16())
    let cursorVisible = u8() != 0
    let cursorStyle = Int(u8())
    let cursorBlinking = u8() != 0
    _ = u8()
    let foreground = u32()
    let background = u32()
    let cursorColor = u32()

    let cellCount = cols * rows
    var foregrounds = [UInt32](repeating: 0, count: cellCount)
    var backgrounds = [UInt32](repeating: 0, count: cellCount)
    var flags = [UInt16](repeating: 0, count: cellCount)
    var text = [String](repeating: "", count: cellCount)
    for index in 0..<cellCount {
      guard bytes.count - offset >= cellHeaderBytes else { return nil }
      foregrounds[index] = u32()
      backgrounds[index] = u32()
      flags[index] = u16()
      let length = Int(u16())
      guard bytes.count - offset >= length else { return nil }
      if length == 1, bytes[offset] < 0x80 {
        text[index] = asciiText[Int(bytes[offset])]
      } else if length > 0 {
        text[index] = String(
          decoding: UnsafeRawBufferPointer(rebasing: bytes[offset..<(offset + length)]),
          as: UTF8.self)
      }
      offset += length
    }

    return TerminalFrame(
      cols: cols,
      rows: rows,
      cursorX: cursorX,
      cursorY: cursorY,
      cursorVisible: cursorVisible,
      cursorStyle: cursorStyle,
      cursorBlinking: cursorBlinking,
      foreground: foreground,
      background: background,
      cursorColor: cursorColor,
      cellForegrounds: foregrounds,
      cellBackgrounds: backgrounds,
      cellFlags: flags,
      cellText: text)
  }
}
