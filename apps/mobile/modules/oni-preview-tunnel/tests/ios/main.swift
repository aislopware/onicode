var failures = 0
func expect(_ actual: String, _ expected: String, _ label: String) {
  guard actual != expected else { return }
  failures += 1
  print("FAIL \(label):\n  got      \(actual.debugDescription)\n  expected \(expected.debugDescription)")
}

/// Feeds `requests` then `responses` in the given pieces, and returns what each side receives.
func exchange(_ requests: [String], _ responses: [String]) -> (server: String, page: String) {
  let (toServer, toPage) = OniHttpRewriter.pair()
  var server = Data()
  for piece in requests { server += toServer.feed(Data(piece.utf8)) }
  server += toServer.finish()
  var page = Data()
  for piece in responses { page += toPage.feed(Data(piece.utf8)) }
  page += toPage.finish()
  return (String(decoding: server, as: UTF8.self), String(decoding: page, as: UTF8.self))
}

/// Every way to split `text` in two, so a match or a head split across reads is covered.
func splits(_ text: String) -> [[String]] {
  let bytes = Array(text.utf8)
  return (0...bytes.count).map {
    [String(decoding: bytes[..<$0], as: UTF8.self), String(decoding: bytes[$0...], as: UTF8.self)]
  }
}

let get = "GET / HTTP/1.1\r\nHost: onil.test:5173\r\n\r\n"

let request = "GET / HTTP/1.1\r\nHost: app.onil.test:5173\r\naccept-encoding: gzip\r\n\r\n"
for pieces in splits(request) {
  expect(
    exchange(pieces, []).server,
    "GET / HTTP/1.1\r\nHost: app.localhost:5173\r\nAccept-Encoding: identity\r\n\r\n",
    "request head split at \(pieces[0].utf8.count)"
  )
}

let script = "fetch('http://127.0.0.1:3001/a'); fetch('http://localhost:3001/b'); 127.0.0.12"
let scriptHead = "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: \(script.utf8.count)\r\n\r\n"
for pieces in splits(scriptHead + script) {
  expect(
    exchange([get], pieces).page,
    scriptHead + "fetch('http://onil.test:3001/a'); fetch('http://onil.test:3001/b'); 127.0.0.12",
    "text body split at \(pieces[0].utf8.count)"
  )
}

// A body ending in an address is rewritten once the message ends, and one running into more
// digits in the next read is not.
let ip = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n"
expect(exchange([get], [ip + "at 127.0.0.1"]).page, ip + "at onil.test", "address ends the body")
expect(exchange([get], [ip + "at 127.0.0.1", "2"]).page, ip + "at 127.0.0.12", "address runs on")

let binary = "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 18\r\n\r\nlocalhost127.0.0.1"
expect(exchange([get], [binary]).page, binary, "binary body passes unchanged")
let gzip = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Encoding: gzip\r\nContent-Length: 9\r\n\r\nlocalhost"
expect(exchange([get], [gzip]).page, gzip, "encoded body passes unchanged")

// Keep-alive: the next response's head is found after a chunked body and after a HEAD reply.
let chunked = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\n\r\n9\r\nlocalhost\r\n0\r\n\r\n"
let redirect = "HTTP/1.1 302 Found\r\nLocation: http://localhost:3001/\r\nContent-Length: 0\r\n\r\n"
for pieces in splits(chunked + redirect) {
  expect(
    exchange([get, get], pieces).page,
    chunked.replacingOccurrences(of: "\nlocalhost", with: "\nonil.test")
      + redirect.replacingOccurrences(of: "localhost", with: "onil.test"),
    "chunked then redirect split at \(pieces[0].utf8.count)"
  )
}
let head = "HEAD / HTTP/1.1\r\nHost: onil.test:5173\r\n\r\n"
let headReply = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 9\r\n\r\n"
expect(
  exchange([head, get], [headReply + headReply + "localhost"]).page,
  headReply + headReply + "onil.test",
  "a HEAD reply has no body"
)

// After an upgrade the bytes are WebSocket frames, not HTTP.
let upgrade = "GET /hmr HTTP/1.1\r\nHost: onil.test:5173\r\nUpgrade: websocket\r\n\r\n"
let switched = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n"
expect(exchange([upgrade], [switched + "localhost"]).page, switched + "localhost", "upgraded response")

// A match split across HTTP chunks is still one match in the body.
let chunkedHead = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\n\r\n"
let splitChunks = "5\r\nlocal\r\n4\r\nhost\r\n0\r\n\r\n"
for pieces in splits(chunkedHead + splitChunks) {
  expect(
    exchange([get], pieces).page,
    chunkedHead + "5\r\nonil.\r\n4\r\ntest\r\n0\r\n\r\n",
    "match across chunks split at \(pieces[0].utf8.count)"
  )
}
let runOn = "9\r\n127.0.0.1\r\n1\r\n2\r\n0\r\n\r\n"
expect(exchange([get], [chunkedHead + runOn]).page, chunkedHead + runOn, "address runs on into the next chunk")

// Repeated identical lengths are one length; anything else unreadable is not HTTP to rewrite.
let twice = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 9\r\nContent-Length: 9\r\n\r\nlocalhost"
expect(exchange([get, get], [twice + binary]).page, twice.replacingOccurrences(of: "\nlocalhost", with: "\nonil.test") + binary, "repeated content length")
let unreadable = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 9, 8\r\n\r\nlocalhost"
expect(exchange([get, get], [unreadable + binary]).page, unreadable + binary, "unreadable content length")

// Head bytes other than the swaps are kept, even when they are not UTF-8.
let latin = Data("HTTP/1.1 200 OK\r\nContent-Disposition: attachment; filename=\"caf".utf8) + Data([0xE9])
  + Data("\"\r\nContent-Length: 0\r\n\r\n".utf8)
expect(OniHttpRewriter.pair().toPage.feed(latin).base64EncodedString(), latin.base64EncodedString(), "non-UTF-8 head")

// `localhost` inside a longer name is another host.
let names = "mylocalhost.dev localhost_x localhost:1 app.localhost/"
expect(
  exchange([get], [ip + names]).page,
  ip + "mylocalhost.dev localhost_x onil.test:1 app.onil.test/",
  "localhost inside a name"
)

// Only text types are rewritten, not types that merely mention one, nor transfer-coded bodies.
for type in ["application/vnd.openxmlformats-officedocument.wordprocessingml.document", "application/jsonx-binary"] {
  let office = "HTTP/1.1 200 OK\r\nContent-Type: \(type)\r\nContent-Length: 9\r\n\r\nlocalhost"
  expect(exchange([get], [office]).page, office, "\(type) passes unchanged")
}
let gzipped = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: gzip, chunked\r\n\r\n9\r\nlocalhost\r\n0\r\n\r\n"
expect(exchange([get], [gzipped]).page, gzipped, "transfer-coded body passes unchanged")
let svg = "HTTP/1.1 200 OK\r\nContent-Type: image/svg+xml; charset=utf-8\r\nContent-Length: 9\r\n\r\nlocalhost"
expect(exchange([get], [svg]).page, svg.replacingOccurrences(of: "\nlocalhost", with: "\nonil.test"), "+xml type is text")

// A chunk size that is not one leaves the rest unread rather than trusted.
let negative = chunkedHead + "-5\r\nlocalhost"
expect(exchange([get], [negative]).page, negative, "negative chunk size")

/// Sends each piece to the server (true) or the page (false), in order, like a real connection.
func converse(_ steps: [(Bool, String)]) -> (server: String, page: String) {
  let (toServer, toPage) = OniHttpRewriter.pair()
  var server = Data(), page = Data()
  for (up, piece) in steps {
    if up { server += toServer.feed(Data(piece.utf8)) } else { page += toPage.feed(Data(piece.utf8)) }
  }
  return (String(decoding: server, as: UTF8.self), String(decoding: page, as: UTF8.self))
}
// Only a server's 101 ends HTTP on a connection; a declined upgrade leaves it HTTP both ways.
let declined = converse([(true, upgrade), (false, "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"), (true, get)])
expect(declined.server, (upgrade + get).replacingOccurrences(of: "onil.test", with: "localhost"), "declined upgrade")
let accepted = converse([(true, upgrade), (false, switched), (true, "onil.test")])
expect(accepted.server, upgrade.replacingOccurrences(of: "onil.test", with: "localhost") + "onil.test", "accepted upgrade")

func standIn(_ url: String) -> String { OniLoopback.standIn(URL(string: url)!).absoluteString }
func shown(_ url: String) -> String { OniLoopback.shown(URL(string: url)!).absoluteString }
expect(standIn("http://localhost:5173/x"), "http://onil.test:5173/x", "standIn localhost")
expect(standIn("http://App.LocalHost.:5173/x"), "http://app.onil.test:5173/x", "standIn subdomain")
expect(standIn("http://localhost.:5173/"), "http://onil.test:5173/", "standIn trailing dot")
expect(standIn("http://[::1]:5173/"), "http://onil.test:5173/", "standIn ipv6")
expect(standIn("http://notlocalhost:5173/"), "http://notlocalhost:5173/", "standIn other host")
expect(shown("http://a.b.onil.test:1/p"), "http://a.b.localhost:1/p", "shown subdomain")
expect(shown("http://xonil.test:1/p"), "http://xonil.test:1/p", "shown other host")

if failures > 0 { exit(1) }
print("tunnel tests passed")
