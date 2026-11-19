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
expect(exchange([upgrade, "onil.test"], []).server, upgrade.replacingOccurrences(of: "onil.test", with: "localhost") + "onil.test", "upgraded request")
expect(exchange([upgrade], [switched + "localhost"]).page, switched + "localhost", "upgraded response")

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
