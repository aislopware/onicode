import Foundation

/// One direction of a plain HTTP/1.1 connection between the page and the environment, with
/// `localhost` spelled the stand-in way on the page's side. Heads are rewritten whole. Unencoded
/// text bodies are too, so a page's absolute URLs to the host's other ports reach it and its
/// `location.hostname === "localhost"` checks still hold; other bodies pass byte for byte, and so
/// does everything after a protocol upgrade. Tested by tests/run-ios.sh.
final class OniHttpRewriter {
  enum Role { case request, response }

  struct Rule {
    let from: [UInt8]
    let to: [UInt8]
    /// Bytes that may not touch a match on either side, so `127.0.0.12` is not `127.0.0.1`.
    let alone: [UInt8]

    init(from: String, to: String, alone: [UInt8] = []) {
      self.from = Array(from.utf8)
      self.to = Array(to.utf8)
      self.alone = alone
      // A body keeps its Content-Length or chunk sizes only when no swap changes its length.
      precondition(self.from.count == self.to.count, "a swap must keep message lengths")
    }
  }

  /// What the two directions of one connection share.
  final class Exchange {
    /// The methods of requests still waiting for a response, so a reply to HEAD has no body.
    fileprivate var pending: [String] = []
    /// Set by a 101 reply; a request asking to upgrade stays HTTP until the server agrees.
    fileprivate var upgraded = false
  }

  private enum Body {
    case length(Int)
    case chunkSize
    case chunkData(Int)
    case chunkEnd(Int)
    case trailers
    case untilClose
  }

  private enum State {
    case head
    case body(Body, text: Bool)
    /// After an upgrade, or a head too large to be HTTP: bytes pass unread.
    case raw
  }

  private static let maxHead = 64 * 1024
  private static let headEnd = Array("\r\n\r\n".utf8)
  private static let lineEnd = Array("\r\n".utf8)

  private let role: Role
  private let rules: [Rule]
  private let exchange: Exchange
  private var state = State.head
  /// Bytes not read yet: an unfinished head, chunk-size line or trailer section.
  private var pending: [UInt8] = []
  /// The end of a text body, already counted, that may start a match the next read completes.
  private var held: [UInt8] = []
  /// The text body byte sent just before `held`, which a match's start must not touch.
  private var previous: UInt8?
  /// Chunk framing read after `held` began, sent at its offset in `held` once `held` is.
  private var framing: [(at: Int, bytes: [UInt8])] = []

  init(role: Role, rules: [Rule], exchange: Exchange) {
    self.role = role
    self.rules = rules
    self.exchange = exchange
  }

  /// The page names the stand-in host, and the environment's servers expect `localhost`.
  static func pair() -> (toServer: OniHttpRewriter, toPage: OniHttpRewriter) {
    let exchange = Exchange()
    let nameBytes = Array("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_".utf8)
    return (
      OniHttpRewriter(
        role: .request,
        rules: [.init(from: OniLoopback.standInHost, to: "localhost")],
        exchange: exchange
      ),
      OniHttpRewriter(
        role: .response,
        rules: [
          .init(from: "localhost", to: OniLoopback.standInHost, alone: nameBytes),
          .init(from: "127.0.0.1", to: OniLoopback.standInHost, alone: Array("0123456789.".utf8)),
        ],
        exchange: exchange
      )
    )
  }

  func feed(_ data: Data) -> Data {
    if role == .request, exchange.upgraded { state = .raw }
    var input = pending + [UInt8](data)
    pending = []
    var output: [UInt8] = []
    while !input.isEmpty {
      let consumed = step(input, into: &output)
      if consumed == 0, case .raw = state { continue }
      if consumed == 0 { break }
      input.removeFirst(consumed)
    }
    pending = input
    return Data(output)
  }

  /// What is still held back, once the stream has ended.
  func finish() -> Data {
    let output = flush() + pending
    pending = []
    return Data(output)
  }

  /// Consumes a prefix of `input`, or nothing when it needs more bytes, which it then holds.
  private func step(_ input: [UInt8], into output: inout [UInt8]) -> Int {
    switch state {
    case .raw:
      output += input
      return input.count
    case .head:
      guard let end = find(Self.headEnd, in: input) else { return hold(input) }
      let head = Array(input[..<(end + Self.headEnd.count)])
      output += readHead(head)
      return head.count
    case .body(let body, let text):
      return stepBody(body, text: text, input, into: &output)
    }
  }

  private func stepBody(_ body: Body, text: Bool, _ input: [UInt8], into output: inout [UInt8])
    -> Int
  {
    switch body {
    case .length(let remaining), .chunkData(let remaining):
      let count = min(remaining, input.count)
      let left = remaining - count
      let done = left == 0
      // A chunk's end is not the body's, so a match may carry on into the next chunk.
      let ends: Bool
      if case .length = body { ends = done } else { ends = false }
      output += bodyBytes(Array(input[..<count]), text: text, ends: ends)
      if case .length = body {
        state = done ? .head : .body(.length(left), text: text)
      } else {
        state = .body(done ? .chunkEnd(2) : .chunkData(left), text: text)
      }
      return count
    case .untilClose:
      output += bodyBytes(input, text: text, ends: false)
      return input.count
    case .chunkEnd(let remaining):
      let count = min(remaining, input.count)
      frame(Array(input[..<count]), into: &output)
      state = .body(remaining == count ? .chunkSize : .chunkEnd(remaining - count), text: text)
      return count
    case .chunkSize:
      guard let end = find(Self.lineEnd, in: input) else { return hold(input) }
      let line = Array(input[..<(end + 2)])
      frame(line, into: &output)
      let digits = String(decoding: line.prefix { $0 != UInt8(ascii: ";") && $0 != 13 }, as: UTF8.self)
      let size = Int(digits.trimmingCharacters(in: .whitespaces), radix: 16).flatMap {
        $0 >= 0 ? $0 : nil
      }
      if size == nil || size == 0 { output += flush() }
      guard let size else {
        state = .raw
        return line.count
      }
      state = .body(size == 0 ? .trailers : .chunkData(size), text: text)
      return line.count
    case .trailers:
      // Trailer lines, if any, then an empty line; the empty line alone is the usual case.
      if input.starts(with: Self.lineEnd) {
        output += Self.lineEnd
        state = .head
        return 2
      }
      guard let end = find(Self.headEnd, in: input) else { return hold(input) }
      output += input[..<(end + 4)]
      state = .head
      return end + 4
    }
  }

  /// Waits for more of a head or framing line, unless it is too long to be one.
  private func hold(_ input: [UInt8]) -> Int {
    guard input.count > Self.maxHead else { return 0 }
    state = .raw
    return 0
  }

  /// A text body's bytes, holding back an end that may start a match the next read completes.
  private func bodyBytes(_ bytes: [UInt8], text: Bool, ends: Bool) -> [UInt8] {
    guard text else { return bytes }
    let buffer = held + bytes
    let keep = ends ? 0 : heldBack(buffer)
    let (rewritten, used) = rewrite(
      buffer, previous: previous, next: nil, limit: buffer.count - keep, atEnd: ends)
    held = Array(buffer[used...])
    if let last = rewritten.last { previous = last }
    if ends { previous = nil }
    return interleave(rewritten)
  }

  /// Sends what is held as the end of its body.
  private func flush() -> [UInt8] {
    let data = rewrite(held, previous: previous, next: nil, limit: held.count).bytes
    held = []
    previous = nil
    return interleave(data)
  }

  /// Chunk framing goes out in place, after the body bytes held before it.
  private func frame(_ bytes: [UInt8], into output: inout [UInt8]) {
    if held.isEmpty {
      output += bytes
    } else {
      framing.append((held.count, bytes))
    }
  }

  /// `data`, the start of `held` rewritten, with the framing read inside it put back.
  private func interleave(_ data: [UInt8]) -> [UInt8] {
    var output: [UInt8] = []
    var start = 0
    while let piece = framing.first, piece.at <= data.count {
      output += data[start..<piece.at]
      output += piece.bytes
      start = piece.at
      framing.removeFirst()
    }
    output += data[start...]
    framing = framing.map { ($0.at - data.count, $0.bytes) }
    return output
  }

  /// The longest end of `bytes` that a rule could still match from, including a whole match
  /// whose following byte has not arrived.
  private func heldBack(_ bytes: [UInt8]) -> Int {
    var held = 0
    for rule in rules {
      let longest = min(rule.alone.isEmpty ? rule.from.count - 1 : rule.from.count, bytes.count)
      for length in stride(from: longest, to: held, by: -1)
      where bytes.suffix(length).elementsEqual(rule.from.prefix(length)) {
        held = length
        break
      }
    }
    return held
  }

  private func readHead(_ head: [UInt8]) -> [UInt8] {
    // Bytes, not a String, so values that are not UTF-8 pass unchanged.
    var lines = split(Array(head.dropLast(Self.headEnd.count)))
    let text = { (line: [UInt8]) in String(decoding: line, as: UTF8.self) }
    let start = lines.first.map(text) ?? ""
    var fields: [String: String] = [:]
    for line in lines.dropFirst().map(text) {
      guard let colon = line.firstIndex(of: ":") else { continue }
      let name = line[..<colon].lowercased()
      let value = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
      fields[name] = fields[name].map { "\($0), \(value)" } ?? value
    }
    let isText = isText(fields)
    let chunked = fields["transfer-encoding"]?.lowercased().contains("chunked") == true
    // Repeats of one length are that length; any other unreadable one leaves the framing unknown.
    let lengths = Set(
      (fields["content-length"] ?? "").split(separator: ",").map {
        Int($0.trimmingCharacters(in: .whitespaces)) ?? -1
      })
    let length = lengths.count == 1 ? lengths.first.flatMap { $0 >= 0 ? $0 : nil } : nil
    let unreadable = !chunked && fields["content-length"] != nil && length == nil
    switch role {
    case .request:
      exchange.pending.append(String(start.prefix { $0 != " " }).uppercased())
      if unreadable {
        state = .raw
      } else if chunked {
        state = .body(.chunkSize, text: isText)
      } else {
        state = length.map { $0 > 0 ? .body(.length($0), text: isText) : .head } ?? .head
      }
      // An encoded reply would hide its loopback URLs from the rewrite.
      lines = lines.map {
        text($0).lowercased().hasPrefix("accept-encoding:")
          ? Array("Accept-Encoding: identity".utf8) : $0
      }
    case .response:
      let status = Int(start.split(separator: " ").dropFirst().first ?? "") ?? 0
      if status == 101 {
        exchange.upgraded = true
        state = .raw
      } else if (100..<200).contains(status) {
        state = .head
      } else {
        let method = exchange.pending.isEmpty ? "" : exchange.pending.removeFirst()
        if method == "HEAD" || status == 204 || status == 304 {
          state = .head
        } else if unreadable {
          state = .raw
        } else if chunked {
          state = .body(.chunkSize, text: isText)
        } else if let length {
          state = length > 0 ? .body(.length(length), text: isText) : .head
        } else {
          state = .body(.untilClose, text: isText)
        }
      }
    }
    let rebuilt = Array(lines.joined(separator: Self.lineEnd)) + Self.headEnd
    previous = nil
    return rewrite(rebuilt, previous: nil, next: nil, limit: rebuilt.count, atEnd: true).bytes
  }

  private func isText(_ fields: [String: String]) -> Bool {
    let encoding = fields["content-encoding"]?.lowercased() ?? "identity"
    let transfer = fields["transfer-encoding"]?.lowercased() ?? "chunked"
    guard encoding == "identity", transfer.trimmingCharacters(in: .whitespaces) == "chunked",
      let value = fields["content-type"]?.lowercased()
    else { return false }
    let type = value.prefix { $0 != ";" }.trimmingCharacters(in: .whitespaces)
    return type.hasPrefix("text/") || type.hasSuffix("+json") || type.hasSuffix("+xml")
      || [
        "application/javascript", "application/ecmascript", "application/json",
        "application/xml", "application/x-www-form-urlencoded",
      ].contains(type)
  }

  /// Swaps matches starting before `limit`. A match touching an edge that is not the message's
  /// own (`atEnd` false) is left alone, since the byte past it is unknown.
  private func rewrite(
    _ bytes: [UInt8], previous: UInt8?, next: UInt8?, limit: Int, atEnd: Bool = true
  ) -> (bytes: [UInt8], used: Int) {
    var output = bytes
    var index = 0
    scan: while index < limit {
      for rule in rules {
        let end = index + rule.from.count
        guard end <= output.count, output[index..<end].elementsEqual(rule.from) else { continue }
        if !rule.alone.isEmpty {
          let before = index > 0 ? output[index - 1] : previous
          let after = end < output.count ? output[end] : next
          if end == output.count && !atEnd { continue }
          if let before, rule.alone.contains(before) { continue }
          if let after, rule.alone.contains(after) { continue }
        }
        output.replaceSubrange(index..<end, with: rule.to)
        index = end
        continue scan
      }
      index += 1
    }
    return (Array(output[..<index]), index)
  }

  private func split(_ bytes: [UInt8]) -> [[UInt8]] {
    var lines: [[UInt8]] = []
    var rest = bytes[...]
    while let end = find(Self.lineEnd, in: Array(rest)) {
      lines.append(Array(rest.prefix(end)))
      rest = rest.dropFirst(end + Self.lineEnd.count)
    }
    return lines + [Array(rest)]
  }

  private func find(_ needle: [UInt8], in haystack: [UInt8]) -> Int? {
    guard haystack.count >= needle.count else { return nil }
    for start in 0...(haystack.count - needle.count)
    where haystack[start..<(start + needle.count)].elementsEqual(needle) {
      return start
    }
    return nil
  }
}
