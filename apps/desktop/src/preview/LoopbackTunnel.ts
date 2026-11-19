// Renders another environment's localhost pages in a local webview: the partition's loopback
// traffic goes to an HTTP proxy here, and each connection rides one WebSocket to the
// environment's /api/preview/tunnel. The page keeps its localhost origin.
import * as NodeSocket from "@effect/platform-node/NodeSocket";
import * as NodeSocketServer from "@effect/platform-node/NodeSocketServer";
import * as NodeWS from "@effect/platform-node/NodeWS";
import type { DesktopPreviewTunnelAccessRequest } from "@onicode/contracts";
import type { NonEmptyReadonlyArray } from "effect/Array";
import * as Context from "effect/Context";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as NetAddress from "effect/net/NetAddress";
import * as Option from "effect/Option";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Semaphore from "effect/Semaphore";
import * as Socket from "effect/socket/Socket";

import * as ElectronApp from "../electron/ElectronApp.ts";
import * as ElectronWindow from "../electron/ElectronWindow.ts";
import { PREVIEW_TUNNEL_ACCESS_REQUEST_CHANNEL } from "../ipc/channels.ts";

const ACCESS_TIMEOUT = "15 seconds";
const CONNECT_TIMEOUT = "10 seconds";
const MAX_HEAD_BYTES = 64 * 1024;
const HEAD_END = Buffer.from("\r\n\r\n");
const PROXY_USER = "onicode";
const PROXY_REALM = "Onicode preview";

const REFUSAL_REASONS = {
  400: "Bad Request",
  403: "Forbidden",
  407: "Proxy Authentication Required",
  501: "Not Implemented",
  502: "Bad Gateway",
} as const;

const refusal = (status: keyof typeof REFUSAL_REASONS) =>
  Buffer.from(
    `HTTP/1.1 ${status} ${REFUSAL_REASONS[status]}\r\n` +
      (status === 407 ? `Proxy-Authenticate: Basic realm="${PROXY_REALM}"\r\n` : "") +
      "Content-Length: 0\r\nConnection: close\r\n\r\n",
    "latin1",
  );

const CONNECT_ESTABLISHED = Buffer.from("HTTP/1.1 200 Connection Established\r\n\r\n", "latin1");

const isLoopbackIp = (host: string) =>
  Result.match(NetAddress.ipFromString(host), {
    onSuccess: NetAddress.isLoopback,
    onFailure: () => false,
  });

// A fully qualified `localhost.` keeps its trailing dot.
export const isLoopbackName = (typed: string) => {
  const name = typed.replace(/^\[|\]$/g, "").replace(/\.$/, "");
  return name === "localhost" || name.endsWith(".localhost") || isLoopbackIp(name);
};

// `*://` matches only http and https, so WebSocket schemes are listed separately.
const UNSPECIFIED_ADDRESS_URLS = ["http", "https", "ws", "wss"].flatMap((scheme) => [
  `${scheme}://0.0.0.0/*`,
  `${scheme}://[::]/*`,
]);

const sendUnspecifiedToLocalhost = (
  details: Electron.OnBeforeRequestListenerDetails,
  callback: (response: Electron.CallbackResponse) => void,
) => {
  const url = new URL(details.url);
  // A WebSocket handshake can't be redirected.
  if (url.protocol === "ws:" || url.protocol === "wss:") return callback({ cancel: true });
  url.hostname = "localhost";
  callback({ redirectURL: url.toString() });
};

/**
 * Reads one HTTP message head. `head` is undefined when the bytes are not an HTTP message or the
 * head is too long; `rest` then holds everything read.
 */
const readHead = Effect.fn("LoopbackTunnel.readHead")(function* (
  pull: Effect.Effect<NonEmptyReadonlyArray<Uint8Array>, Socket.SocketError>,
  initial: Buffer = Buffer.alloc(0),
) {
  let received = initial;
  while (true) {
    const end = received.indexOf(HEAD_END);
    if (end > MAX_HEAD_BYTES) return { head: undefined, rest: received };
    if (end >= 0) {
      return {
        head: received.subarray(0, end).toString("latin1"),
        rest: received.subarray(end + 4),
      };
    }
    // Every request line and status line starts with an uppercase token.
    const first = received[0];
    if (
      (first !== undefined && (first < 0x41 || first > 0x5a)) ||
      received.length > MAX_HEAD_BYTES
    ) {
      return { head: undefined, rest: received };
    }
    received = Buffer.concat([received, ...(yield* pull)]);
  }
});

const HOP_BY_HOP_FIELD = /^(?:connection|keep-alive|proxy-authorization|proxy-connection)\s*:/i;

// Constant time, so the response time doesn't reveal how much of a guess matched.
const sameSecret = (offered: string, expected: string) => {
  let difference = offered.length ^ expected.length;
  for (let index = 0; index < expected.length; index++) {
    difference |= offered.charCodeAt(index) ^ expected.charCodeAt(index);
  }
  return difference === 0;
};

type ProxyRequest =
  | { readonly refusal: keyof typeof REFUSAL_REASONS }
  | {
      readonly host: string;
      readonly port: number;
      /** The origin-form head to send on; undefined for CONNECT, which tunnels raw bytes. */
      readonly forwardHead: string | undefined;
      readonly bodyLength: number;
    };

const fieldValues = (fields: ReadonlyArray<string>, name: RegExp) =>
  fields.filter((field) => name.test(field)).map((field) => field.replace(/^[^:]*:\s*/, "").trim());

// The request target as Chromium sent it, minus scheme and authority; URL would re-escape it.
const originForm = (target: string) => {
  const authorityEnd = target.slice("http://".length).search(/[/?]/);
  if (authorityEnd < 0) return "/";
  const path = target.slice("http://".length + authorityEnd);
  return path.startsWith("?") ? `/${path}` : path;
};

/**
 * Chromium sends CONNECT for https and WebSockets, and an absolute-form request for plain http.
 * A forwarded request is rewritten to the origin form with `Connection: close`, so each proxy
 * connection carries one request and the next one is checked and routed again.
 */
const parseProxyRequest = (head: string, authorization: string): ProxyRequest => {
  const [requestLine = "", ...fields] = head.split("\r\n");
  const [method, target, version, extra] = requestLine.split(" ");
  if (!method || !target || !version?.startsWith("HTTP/1.") || extra !== undefined) {
    return { refusal: 400 };
  }
  const [offered] = fieldValues(fields, /^proxy-authorization\s*:/i);
  if (offered === undefined || !sameSecret(offered, authorization)) return { refusal: 407 };
  const connect = method === "CONNECT";
  const url = URL.parse(connect ? `http://${target}` : target);
  if (url === null || url.protocol !== "http:") return { refusal: 400 };
  const host = url.hostname.replace(/^\[|\]$/g, "");
  if (!isLoopbackName(host)) return { refusal: 403 };
  const port = Number(url.port || 80);
  if (connect) return { host, port, forwardHead: undefined, bodyLength: 0 };
  // Only the one framed request is forwarded, so its body length must be known up front.
  if (fieldValues(fields, /^transfer-encoding\s*:/i).length > 0) return { refusal: 501 };
  const lengths = new Set(fieldValues(fields, /^content-length\s*:/i));
  const [length = "0"] = lengths;
  if (lengths.size > 1 || !/^\d+$/.test(length)) return { refusal: 400 };
  return {
    host,
    port,
    bodyLength: Number(length),
    forwardHead: [
      `${method} ${originForm(target)} ${version}`,
      ...fields.filter((field) => !HOP_BY_HOP_FIELD.test(field)),
      "Connection: close",
      "",
      "",
    ].join("\r\n"),
  };
};

const statusCode = (head: string) => Number(head.split(" ", 2)[1]);

// Without this Chromium would keep the proxy connection open and send its next request raw. A
// destination's 407 would look like the listener's own challenge, answered with its credentials.
const closingResponseHead = (head: string) => {
  const [statusLine = "", ...fields] = head.split("\r\n");
  const challenged = statusCode(head) === 407;
  return Buffer.from(
    [
      challenged ? "HTTP/1.1 502 Bad Gateway" : statusLine,
      ...fields.filter(
        (field) =>
          !HOP_BY_HOP_FIELD.test(field) && !(challenged && /^proxy-authenticate\s*:/i.test(field)),
      ),
      "Connection: close",
      "",
      "",
    ].join("\r\n"),
    "latin1",
  );
};

// Over an SSH forward the environment's own origin is a loopback port here, and signed asset
// URLs point at it. That port is the environment, not one of its loopback servers.
const tunnelEndpoint = (tunnelUrl: string) => {
  const url = new URL(tunnelUrl);
  return {
    host: url.hostname.replace(/^\[|\]$/g, ""),
    port: Number(url.port || (url.protocol === "wss:" ? 443 : 80)),
  };
};

const isTunnelEndpoint = (
  tunnelUrl: string,
  destination: { readonly host: string; readonly port: number },
) => {
  const endpoint = tunnelEndpoint(tunnelUrl);
  return (
    isLoopbackName(endpoint.host) &&
    endpoint.host === destination.host &&
    endpoint.port === destination.port
  );
};

/** Opens a connection to the destination; `ws` pauses its socket, so a slow page slows the tunnel. */
const dialDestination = Effect.fn("LoopbackTunnel.dialDestination")(function* (
  tunnelUrl: string,
  destination: { readonly host: string; readonly port: number },
) {
  if (isTunnelEndpoint(tunnelUrl, destination)) {
    const endpoint = yield* NodeSocket.makeNet({
      ...tunnelEndpoint(tunnelUrl),
      openTimeout: CONNECT_TIMEOUT,
    });
    return { pull: yield* Socket.readerBytes(endpoint), writer: yield* endpoint.writer };
  }
  const url = new URL(tunnelUrl);
  url.searchParams.set("port", String(destination.port));
  // A name is the environment's `localhost`; an address must reach that exact address there.
  if (isLoopbackIp(destination.host)) url.searchParams.set("host", destination.host);
  const tunnel = yield* Effect.acquireRelease(
    Effect.sync(() =>
      // Closing before the handshake finishes emits an error after Socket's listeners are gone.
      new NodeWS.WebSocket(url).on("error", () => undefined),
    ),
    (tunnel) => Effect.sync(() => tunnel.close()),
  );
  const socket = yield* Socket.fromWebSocket(Effect.succeed(tunnel), {
    openTimeout: CONNECT_TIMEOUT,
  });
  // Effect's WebSocket writer never waits for the send buffer; `ws` calls back once it is written.
  const sendAll = (chunks: NonEmptyReadonlyArray<Uint8Array>) =>
    Effect.callback<void>((resume) => {
      const last = chunks.length - 1;
      chunks.forEach((chunk, index) =>
        tunnel.send(chunk, index === last ? () => resume(Effect.void) : undefined),
      );
    });
  return {
    pull: yield* Socket.readerBytes(socket),
    writer: { write: (chunk: Uint8Array) => sendAll([chunk]), writeAll: sendAll },
  };
});

const pump =
  /* Runs for the life of the connection; a span per direction adds no diagnostic value. */
  Effect.fnUntraced(function* (
    pull: Effect.Effect<NonEmptyReadonlyArray<Uint8Array>, Socket.SocketError>,
    writeAll: (
      chunks: NonEmptyReadonlyArray<Uint8Array>,
    ) => Effect.Effect<void, Socket.SocketError>,
  ) {
    while (true) yield* writeAll(yield* pull);
  });

export class LoopbackTunnelRouteError extends Schema.TaggedError<LoopbackTunnelRouteError>()(
  "LoopbackTunnelRouteError",
  { environmentId: Schema.String, cause: Schema.Defect() },
) {
  override get message(): string {
    return `Failed to route preview traffic for environment "${this.environmentId}".`;
  }
}

export class LoopbackTunnel extends Context.Service<
  LoopbackTunnel,
  {
    /** Sends a preview session's loopback traffic to its environment, for the session's lifetime. */
    readonly route: (input: {
      readonly environmentId: string;
      readonly session: Electron.Session;
    }) => Effect.Effect<void, LoopbackTunnelRouteError>;
    readonly resolveAccess: (input: {
      readonly requestId: string;
      readonly url: string | null;
    }) => Effect.Effect<void>;
  }
>()("@onicode/desktop/preview/LoopbackTunnel") {}

interface Listener {
  readonly port: number;
  readonly password: string;
  readonly authorization: string;
}

const make = Effect.gen(function* () {
  const electronApp = yield* ElectronApp.ElectronApp;
  const electronWindow = yield* ElectronWindow.ElectronWindow;
  const random = yield* SecureRandom.SecureRandom;
  const listenerScope = yield* Effect.scope;
  const routing = yield* Semaphore.make(1);
  const pending = new Map<string, Deferred.Deferred<string | null>>();
  const listeners = new Map<string, Listener>();
  const routed = new WeakSet<Electron.Session>();
  let requestCount = 0;

  // Any local process can reach the listener's port, so it answers only clients holding this
  // run's credentials, which main hands to Chromium and nothing else.
  yield* electronApp.on(
    "login",
    (
      event: Electron.Event,
      _webContents: Electron.WebContents | null,
      _details: Electron.AuthenticationResponseDetails,
      authInfo: Electron.AuthInfo,
      callback: (username?: string, password?: string) => void,
    ) => {
      if (!authInfo.isProxy || authInfo.host !== "127.0.0.1") return;
      const listener = [...listeners.values()].find(({ port }) => port === authInfo.port);
      if (listener === undefined) return;
      event.preventDefault();
      callback(PROXY_USER, listener.password);
    },
  );

  // The renderer owns environment credentials and the selected route, so it is asked per
  // connection; it decides how long a ticket is reused.
  const requestAccess = Effect.fn("LoopbackTunnel.requestAccess")(function* (
    environmentId: string,
  ) {
    const window = yield* electronWindow.currentMainOrFirst;
    if (Option.isNone(window) || window.value.isDestroyed()) return null;
    const requestId = `preview-tunnel-${requestCount++}`;
    const answer = yield* Deferred.make<string | null>();
    pending.set(requestId, answer);
    window.value.webContents.send(PREVIEW_TUNNEL_ACCESS_REQUEST_CHANNEL, {
      requestId,
      environmentId,
    } as DesktopPreviewTunnelAccessRequest);
    return yield* Deferred.await(answer).pipe(
      Effect.timeoutOption(ACCESS_TIMEOUT),
      Effect.map(Option.getOrNull),
      Effect.ensuring(Effect.sync(() => pending.delete(requestId))),
    );
  });

  /** Serves one proxy connection; never falls back to a direct local connection. */
  const serve = (environmentId: string, authorization: string) =>
    Effect.fn("LoopbackTunnel.serve")(
      function* (socket: Socket.Socket) {
        const pull = yield* Socket.readerBytes(socket);
        const writer = yield* socket.writer;
        const { head, rest } = yield* readHead(pull);
        const request =
          head === undefined ? { refusal: 400 as const } : parseProxyRequest(head, authorization);
        if ("refusal" in request) return yield* writer.write(refusal(request.refusal));
        const tunnelUrl = yield* requestAccess(environmentId);
        const destination =
          tunnelUrl === null
            ? Option.none()
            : yield* dialDestination(tunnelUrl, request).pipe(Effect.option);
        if (Option.isNone(destination)) {
          // Chromium shows a forwarded request's error status as the page; a bare close is an error.
          if (request.forwardHead === undefined) yield* writer.write(refusal(502));
          return;
        }
        const target = destination.value;
        if (request.forwardHead === undefined) {
          yield* writer.write(CONNECT_ESTABLISHED);
          if (rest.length > 0) yield* target.writer.write(rest);
          return yield* Effect.raceFirst(
            pump(pull, target.writer.writeAll),
            pump(target.pull, writer.writeAll),
          );
        }
        const forwardHead = Buffer.from(request.forwardHead, "latin1");
        // One request goes on; anything Chromium sends after its body never leaves this machine.
        const forwardBody = Effect.gen(function* () {
          let remaining = request.bodyLength;
          const take = (chunk: Uint8Array) => {
            const part = chunk.subarray(0, remaining);
            remaining -= part.length;
            return part;
          };
          yield* target.writer.write(Buffer.concat([forwardHead, take(rest)]));
          while (true) {
            for (const chunk of yield* pull) {
              const part = take(chunk);
              if (part.length > 0) yield* target.writer.write(part);
            }
          }
        });
        const relayResponse = Effect.gen(function* () {
          let received: Buffer = Buffer.alloc(0);
          while (true) {
            const response = yield* readHead(target.pull, received);
            // Unread, the response could keep the connection open for Chromium's next request.
            if (response.head === undefined) return yield* writer.write(refusal(502));
            const status = statusCode(response.head);
            if (status >= 200 || status === 101 || !(status >= 100)) {
              yield* writer.write(
                Buffer.concat([closingResponseHead(response.head), response.rest]),
              );
              break;
            }
            yield* writer.write(Buffer.from(`${response.head}\r\n\r\n`, "latin1"));
            received = response.rest;
          }
          return yield* pump(target.pull, writer.writeAll);
        });
        return yield* Effect.raceFirst(forwardBody, relayResponse);
      },
      Effect.scoped,
      (effect) => Effect.ignoreCause(effect),
    );

  const listen = Effect.fn("LoopbackTunnel.listen")(function* (environmentId: string) {
    const existing = listeners.get(environmentId);
    if (existing !== undefined) return existing.port;
    // Listeners live as long as the app; without this run's credentials they refuse everyone.
    const server = yield* NodeSocketServer.make({ port: 0, host: "127.0.0.1" }).pipe(
      Scope.provide(listenerScope),
      Effect.mapError((cause) => new LoopbackTunnelRouteError({ environmentId, cause })),
    );
    const password = Buffer.from(yield* random.randomBytes(32)).toString("base64url");
    const authorization = `Basic ${Buffer.from(`${PROXY_USER}:${password}`).toString("base64")}`;
    yield* server.run(serve(environmentId, authorization)).pipe(Effect.forkIn(listenerScope));
    const port = (server.address as NetAddress.InetAddress).port;
    listeners.set(environmentId, { port, password, authorization });
    return port;
  });

  const route: LoopbackTunnel["Service"]["route"] = Effect.fn("LoopbackTunnel.route")(function* ({
    environmentId,
    session,
  }) {
    if (routed.has(session)) return;
    const port = yield* listen(environmentId);
    yield* Effect.tryPromise({
      try: () =>
        session.setProxy({
          mode: "fixed_servers",
          proxyRules: `http://127.0.0.1:${port}`,
          // Chromium never asks a PAC script about loopback hosts, so the split is done here:
          // `*` sends everything direct, and the later `<-loopback>` takes loopback back out of
          // that bypass, along with link-local, which the CIDRs return. The last matching rule
          // wins. A fixed proxy has no direct retry.
          proxyBypassRules: "*,<-loopback>,169.254.0.0/16,fe80::/10",
        }),
      catch: (cause) => new LoopbackTunnelRouteError({ environmentId, cause }),
    });
    // The bypass rules don't cover the unspecified address, so it would reach this machine.
    session.webRequest.onBeforeRequest(
      { urls: UNSPECIFIED_ADDRESS_URLS },
      sendUnspecifiedToLocalhost,
    );
    routed.add(session);
  }, routing.withPermits(1));

  const resolveAccess: LoopbackTunnel["Service"]["resolveAccess"] = ({ requestId, url }) => {
    const answer = pending.get(requestId);
    return answer ? Deferred.succeed(answer, url).pipe(Effect.asVoid) : Effect.void;
  };

  return LoopbackTunnel.of({ route, resolveAccess });
});

export const layer = Layer.effect(LoopbackTunnel, make);
