// Renders another environment's localhost pages in a local webview: the partition's loopback
// traffic goes to an HTTP proxy here, and each connection rides one WebSocket to the
// environment's /api/preview/tunnel. The page keeps its localhost origin. Forwarded ports take
// the same route from a plain listener, for any client on this machine.
import * as NodeSocket from "@effect/platform-node/NodeSocket";
import * as NodeSocketServer from "@effect/platform-node/NodeSocketServer";
import * as NodeWS from "@effect/platform-node/NodeWS";
import type {
  DesktopPortForward,
  DesktopPreviewTunnelAccessRequest,
  EnvironmentId,
} from "@onicode/contracts";
import * as HostProcess from "@onicode/shared/HostProcess";
import * as NetService from "@onicode/shared/Net";
import type { NonEmptyReadonlyArray } from "effect/Array";
import * as Context from "effect/Context";
import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as NetAddress from "effect/net/NetAddress";
import * as Option from "effect/Option";
import * as Result from "effect/Result";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";
import * as Semaphore from "effect/Semaphore";
import { ChildProcess, ChildProcessSpawner } from "effect/process";
import * as Socket from "effect/socket/Socket";

import * as ElectronApp from "../electron/ElectronApp.ts";
import * as ElectronWindow from "../electron/ElectronWindow.ts";
import { PREVIEW_TUNNEL_ACCESS_REQUEST_CHANNEL } from "../ipc/channels.ts";

const ACCESS_TIMEOUT = "15 seconds";
const PROBE_TIMEOUT = "3 seconds";
const MAX_FORWARD_CONNECTIONS = 64;
const CONNECT_TIMEOUT = "10 seconds";
const OWNER_LOOKUP_TIMEOUT = "5 seconds";
// Chromium accepts response heads up to this size.
const MAX_HEAD_BYTES = 256 * 1024;
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
    onSuccess: (ip) => NetAddress.isLoopback(NetAddress.toCanonical(ip)),
    onFailure: () => false,
  });

// The environment dials IPv4 loopback, so `::ffff:127.0.0.1` goes to it as `127.0.0.1`.
const canonicalHost = (host: string) =>
  Result.match(NetAddress.ipFromString(host), {
    onSuccess: (ip) => NetAddress.formatIp(NetAddress.toCanonical(ip)),
    onFailure: () => host,
  });

// A fully qualified `localhost.` keeps its trailing dot.
export const isLoopbackName = (typed: string) => {
  const name = typed.replace(/^\[|\]$/g, "").replace(/\.$/, "");
  return name === "localhost" || name.endsWith(".localhost") || isLoopbackIp(name);
};

// The bypass rules miss these hosts, so they would reach this machine. Patterns can't name a
// range, so only ::ffff:127.0.0.1 is listed: other mapped 127/8 addresses fail on macOS, and on
// Linux reach only wildcard listeners, which the page reaches through the LAN address anyway.
// `*://` matches only http and https, so WebSocket schemes are listed separately.
const TUNNEL_ESCAPE_URLS = ["http", "https", "ws", "wss"].flatMap((scheme) =>
  ["0.0.0.0", "[::]", "[::ffff:0:0]", "[::ffff:7f00:1]"].map((host) => `${scheme}://${host}/*`),
);

const keepInTunnel = (
  details: Electron.OnBeforeRequestListenerDetails,
  callback: (response: Electron.CallbackResponse) => void,
) => {
  const url = new URL(details.url);
  // A WebSocket handshake can't be redirected.
  if (url.protocol === "ws:" || url.protocol === "wss:") return callback({ cancel: true });
  const unmapped = canonicalHost(url.hostname.replace(/^\[|\]$/g, ""));
  url.hostname = isLoopbackIp(unmapped) ? unmapped : "localhost";
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
 * A forwarded request is rewritten to the origin form without the proxy's own fields.
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
  const host = canonicalHost(url.hostname.replace(/^\[|\]$/g, ""));
  if (!isLoopbackName(host)) return { refusal: 403 };
  const port = Number(url.port || 80);
  if (connect) return { host, port, forwardHead: undefined, bodyLength: 0 };
  // Bodies are forwarded by length, so the next request on the connection can be found and checked.
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
      "",
      "",
    ].join("\r\n"),
  };
};

// NaN unless the status line is strictly formed; anything looser is refused rather than guessed.
const statusCode = (head: string) =>
  Number(/^HTTP\/1\.[01] (\d{3})(?: |$)/.exec(head.split("\r\n", 1)[0] ?? "")?.[1]);

// A destination's 407 would look like the listener's own challenge, answered with its credentials.
const relayedResponseHead = (head: string) => {
  if (statusCode(head) !== 407) return Buffer.from(`${head}\r\n\r\n`, "latin1");
  const [, ...fields] = head.split("\r\n");
  return Buffer.from(
    [
      "HTTP/1.1 502 Bad Gateway",
      ...fields.filter((field) => !/^proxy-authenticate\s*:/i.test(field)),
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
  // `localhost` names the forward; another address at its port is another server there.
  return (
    isLoopbackName(endpoint.host) &&
    endpoint.port === destination.port &&
    (endpoint.host === destination.host || !isLoopbackIp(destination.host))
  );
};

/** Set by the environment's /api/preview/tunnel when it reached the port and nothing listens. */
const PORT_REFUSED_HEADER = "x-oni-tunnel-port-refused";

/** Opens a connection through the tunnel; `ws` pauses its socket, so a slow page slows the tunnel. */
const dialTunnel = Effect.fn("LoopbackTunnel.dialTunnel")(function* (
  tunnelUrl: string,
  destination: { readonly host: string; readonly port: number },
  onPortRefused?: () => void,
) {
  const url = new URL(tunnelUrl);
  url.searchParams.set("port", String(destination.port));
  // A name is the environment's `localhost`; an address must reach that exact address there.
  if (isLoopbackIp(destination.host)) url.searchParams.set("host", destination.host);
  const tunnel = yield* Effect.acquireRelease(
    Effect.sync(() => {
      // Closing before the handshake finishes emits an error after Socket's listeners are gone.
      const tunnel = new NodeWS.WebSocket(url).on("error", () => undefined);
      if (onPortRefused) {
        tunnel.on("unexpected-response", (_request, response) => {
          if (response.headers[PORT_REFUSED_HEADER] !== undefined) onPortRefused();
          // Handled here, so `ws` leaves aborting the handshake to us.
          tunnel.terminate();
        });
      }
      return tunnel;
    }),
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

const dialDestination = Effect.fn("LoopbackTunnel.dialDestination")(function* (
  tunnelUrl: string,
  destination: { readonly host: string; readonly port: number },
) {
  if (!isTunnelEndpoint(tunnelUrl, destination)) return yield* dialTunnel(tunnelUrl, destination);
  const endpoint = yield* NodeSocket.makeNet({
    ...tunnelEndpoint(tunnelUrl),
    openTimeout: CONNECT_TIMEOUT,
  });
  return { pull: yield* Socket.readerBytes(endpoint), writer: yield* endpoint.writer };
});

const TCP_TABLES = ["/proc/net/tcp", "/proc/net/tcp6"];

// Rows read `sl local_address rem_address st queues timer retransmits uid ...`, with addresses
// as HEXIP:HEXPORT and each 32-bit word of the IP in little-endian order; tcp6 holds IPv4 as
// ::ffff:a.b.c.d. Forwards listen only on 127.0.0.1 and ::1.
const procAddresses = (ip: string, port: number) => {
  const hexPort = port.toString(16).padStart(4, "0").toUpperCase();
  if (ip === "::1") return [`00000000000000000000000001000000:${hexPort}`];
  const octets = ip
    .replace(/^::ffff:/i, "")
    .split(".")
    .map(Number);
  const v4 = octets
    .reverse()
    .map((octet) => octet.toString(16).padStart(2, "0"))
    .join("")
    .toUpperCase();
  return [`${v4}:${hexPort}`, `0000000000000000FFFF0000${v4}:${hexPort}`];
};

/** The owner of the row whose local end is the client and remote end is the listener. */
const connectionOwner = (
  tables: ReadonlyArray<string>,
  client: { readonly address: string; readonly port: number },
  listener: { readonly address: string; readonly port: number },
) => {
  const clientEnds = procAddresses(client.address, client.port);
  const listenerEnds = procAddresses(listener.address, listener.port);
  for (const line of tables.flatMap((table) => table.split("\n").slice(1))) {
    const [, local, remote, , , , , uid] = line.trim().split(/\s+/);
    if (
      local !== undefined &&
      remote !== undefined &&
      clientEnds.includes(local.toUpperCase()) &&
      listenerEnds.includes(remote.toUpperCase())
    ) {
      return Number(uid);
    }
  }
  return undefined;
};

interface ConnectionEnd {
  readonly address: string;
  readonly port: number;
}

/** One end as `lsof` takes and prints it: brackets around IPv6 only. */
const lsofEnd = ({ address, port }: ConnectionEnd) =>
  address.includes(":") ? `[${address}]:${port}` : `${address}:${port}`;

/**
 * The owner of the client's socket in `lsof -F un`, which prints a process as `p<pid>` and
 * `u<uid>`, then each of its sockets as `n<local>-><remote>`. Without root it lists this user's
 * processes only, so another user's client has no row.
 */
const lsofConnectionOwner = (output: string, client: ConnectionEnd, listener: ConnectionEnd) => {
  const socket = `n${lsofEnd(client)}->${lsofEnd(listener)}`;
  let owner: number | undefined;
  for (const line of output.split("\n").map((line) => line.trim())) {
    if (line.startsWith("p")) owner = undefined;
    else if (line.startsWith("u")) owner = Number(line.slice(1));
    else if (line === socket) return owner;
  }
  return undefined;
};

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

export class LoopbackTunnelForwardError extends Schema.TaggedError<LoopbackTunnelForwardError>()(
  "LoopbackTunnelForwardError",
  { environmentId: Schema.String, port: Schema.Number, cause: Schema.Defect() },
) {
  override get message(): string {
    return `Failed to forward port ${this.port} of environment "${this.environmentId}".`;
  }
}

export class LoopbackTunnelUnavailableError extends Schema.TaggedError<LoopbackTunnelUnavailableError>()(
  "LoopbackTunnelUnavailableError",
  { environmentId: Schema.String },
) {
  override get message(): string {
    return `Environment "${this.environmentId}" can't be reached through the tunnel right now.`;
  }
}

export class LoopbackTunnelSameMachineError extends Schema.TaggedError<LoopbackTunnelSameMachineError>()(
  "LoopbackTunnelSameMachineError",
  { environmentId: Schema.String, port: Schema.Number },
) {
  override get message(): string {
    return `This environment runs on this computer, so localhost:${this.port} already reaches it.`;
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
    /** Listens on this machine's loopback, on the same port when it is free. */
    readonly forward: (input: {
      readonly environmentId: EnvironmentId;
      readonly remotePort: number;
    }) => Effect.Effect<
      DesktopPortForward,
      LoopbackTunnelForwardError | LoopbackTunnelSameMachineError | LoopbackTunnelUnavailableError
    >;
    readonly stopForward: (input: {
      readonly environmentId: EnvironmentId;
      readonly remotePort: number;
    }) => Effect.Effect<void>;
    readonly forwards: Effect.Effect<ReadonlyArray<DesktopPortForward>>;
    /** Whether the environment's loopback is this machine's, so forwarding would only loop. */
    readonly sharesLoopback: (
      environmentId: EnvironmentId,
    ) => Effect.Effect<boolean, LoopbackTunnelUnavailableError>;
    readonly subscribeForwards: (
      listener: (forwards: ReadonlyArray<DesktopPortForward>) => Effect.Effect<void>,
    ) => Effect.Effect<void, never, Scope.Scope>;
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
  const fileSystem = yield* FileSystem.FileSystem;
  const platform = yield* HostProcess.Platform;
  const userId = yield* HostProcess.UserId;
  const spawner = yield* ChildProcessSpawner.ChildProcessSpawner;
  const random = yield* SecureRandom.SecureRandom;
  const net = yield* NetService.NetService;
  const listenerScope = yield* Effect.scope;
  const routing = yield* Semaphore.make(1);
  const forwarding = yield* Semaphore.make(1);
  const forwarded = new Map<
    string,
    { readonly forward: DesktopPortForward; readonly scope: Scope.Closeable }
  >();
  // Counts Stops per forward, so a start still probing when Stop came does not bind after it.
  const forwardStops = new Map<string, number>();
  const forwardListeners = new Set<
    (forwards: ReadonlyArray<DesktopPortForward>) => Effect.Effect<void>
  >();
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
        const destinationKey = `${request.host} ${request.port}`;
        let sent = 0;
        let answered = 0;
        // Requests to the same server reuse the tunnel. Each is checked again, sent without the
        // proxy's own fields, and its body is forwarded by length so the next one can be found.
        const forwardRequests = Effect.gen(function* () {
          let next: ProxyRequest = request;
          let buffered = rest;
          while (!("refusal" in next) && next.forwardHead !== undefined) {
            let remaining = next.bodyLength;
            const take = (chunk: Buffer) => {
              const part = chunk.subarray(0, remaining);
              remaining -= part.length;
              buffered = chunk.subarray(part.length);
              return part;
            };
            sent++;
            yield* target.writer.write(
              Buffer.concat([Buffer.from(next.forwardHead, "latin1"), take(buffered)]),
            );
            while (remaining > 0) {
              const part = take(Buffer.concat(yield* pull));
              if (part.length > 0) yield* target.writer.write(part);
            }
            // Chromium sends the next request only once it has read the whole response.
            const following = yield* readHead(pull, buffered);
            if (following.head === undefined) return yield* writer.write(refusal(400));
            next = parseProxyRequest(following.head, authorization);
            if ("refusal" in next) return yield* writer.write(refusal(next.refusal));
            // Another server needs its own tunnel; Chromium retries on a new connection.
            if (`${next.host} ${next.port}` !== destinationKey) return;
            buffered = following.rest;
          }
        });
        const relayResponses = Effect.gen(function* () {
          let received: Buffer = Buffer.alloc(0);
          while (true) {
            // A server gone before the head ends is answered like one that sent nonsense.
            const response = yield* readHead(target.pull, received).pipe(
              Effect.orElseSucceed(() => ({ head: undefined, rest: received })),
            );
            const status = response.head === undefined ? Number.NaN : statusCode(response.head);
            // Unread, it would desynchronise every later response. Nothing of it has reached
            // Chromium, and a bare close would make Chromium send the request again.
            if (response.head === undefined || Number.isNaN(status)) {
              return yield* writer.write(refusal(502));
            }
            if (status >= 100 && status < 200 && status !== 101) {
              yield* writer.write(Buffer.from(`${response.head}\r\n\r\n`, "latin1"));
              received = response.rest;
              continue;
            }
            answered++;
            yield* writer.write(Buffer.concat([relayedResponseHead(response.head), response.rest]));
            // Bytes that arrive after the next request went out start that request's response.
            while (true) {
              const chunks = yield* target.pull;
              if (sent > answered) {
                received = Buffer.concat(chunks);
                break;
              }
              yield* writer.writeAll(chunks);
            }
          }
        });
        return yield* Effect.raceFirst(forwardRequests, relayResponses);
      },
      Effect.scoped,
      (effect) => Effect.ignoreCause(effect),
    );

  /** What the command printed, or nothing when it could not run or took too long. */
  const commandOutput = (command: string, args: ReadonlyArray<string>) =>
    spawner.string(ChildProcess.make(command, args, { stdin: "ignore", stderr: "ignore" })).pipe(
      Effect.timeoutOption(OWNER_LOOKUP_TIMEOUT),
      Effect.map(Option.getOrElse(() => "")),
      Effect.orElseSucceed(() => ""),
    );

  const isOwnLinuxConnection = Effect.fn("LoopbackTunnel.isOwnLinuxConnection")(function* (
    client: ConnectionEnd,
    listener: ConnectionEnd,
  ) {
    const tables = yield* Effect.forEach(TCP_TABLES, (path) =>
      fileSystem.readFileString(path).pipe(Effect.orElseSucceed(() => "")),
    );
    const owner = connectionOwner(tables, client, listener);
    return owner !== undefined && owner === userId;
  });

  const isOwnMacConnection = Effect.fn("LoopbackTunnel.isOwnMacConnection")(function* (
    client: ConnectionEnd,
    listener: ConnectionEnd,
  ) {
    const sockets = yield* commandOutput("/usr/sbin/lsof", [
      "-nP",
      `-iTCP@${lsofEnd(client)}`,
      "-F",
      "un",
    ]);
    const owner = lsofConnectionOwner(sockets, client, listener);
    return owner !== undefined && owner === userId;
  });

  // A forward is plain TCP with no credentials to check, so on Linux and macOS it serves only
  // clients of this OS user, and a connection whose owner cannot be read is refused. Windows and
  // other platforms have no owner check.
  const isOwnConnection = Effect.fn("LoopbackTunnel.isOwnConnection")(function* () {
    if (platform !== "linux" && platform !== "darwin") return true;
    const connection = yield* Effect.serviceOption(NodeSocket.NetSocket);
    if (Option.isNone(connection)) return false;
    const { remoteAddress, remotePort, localAddress, localPort } = connection.value;
    if (!remoteAddress || !remotePort || !localAddress || !localPort) return false;
    // A dual-stack socket reports an IPv4 peer as ::ffff:a.b.c.d; the lookups print it plain.
    const plain = (address: string) => address.replace(/^::ffff:/i, "");
    const client = { address: plain(remoteAddress), port: remotePort };
    const listener = { address: plain(localAddress), port: localPort };
    return platform === "linux"
      ? yield* isOwnLinuxConnection(client, listener)
      : yield* isOwnMacConnection(client, listener);
  });

  /** Serves one connection to a forwarded port; closes it when the environment is out of reach. */
  const serveForward = (environmentId: string, port: number) => {
    let active = 0;
    return Effect.fn("LoopbackTunnel.serveForward")(
      function* (socket: Socket.Socket) {
        // Acquiring the socket ties it to this scope, so returning early closes it.
        const pull = yield* Socket.readerBytes(socket);
        const writer = yield* socket.writer;
        // Closed at once, so a page hammering the port can't queue tunnels and access requests.
        if (active >= MAX_FORWARD_CONNECTIONS) return;
        active++;
        yield* Effect.addFinalizer(() => Effect.sync(() => active--));
        if (!(yield* isOwnConnection())) return;
        const tunnelUrl = yield* requestAccess(environmentId);
        if (tunnelUrl === null) return;
        const destination = yield* dialTunnel(tunnelUrl, { host: "localhost", port });
        return yield* Effect.raceFirst(
          pump(pull, destination.writer.writeAll),
          pump(destination.pull, writer.writeAll),
        );
      },
      Effect.scoped,
      (effect) => Effect.ignoreCause(effect),
    );
  };

  /**
   * Whether the environment's loopback is this machine's, by tunnelling a nonce to a local probe.
   * A forward there would accept the environment's own dials and loop through the tunnel.
   */
  const sharesLoopback = Effect.fn("LoopbackTunnel.sharesLoopback")(function* (
    environmentId: string,
  ) {
    const tunnelUrl = yield* requestAccess(environmentId);
    // Without access the answer is unknown, not "another machine": a forward could loop later.
    if (tunnelUrl === null) return yield* new LoopbackTunnelUnavailableError({ environmentId });
    return yield* probeLoopback(environmentId, tunnelUrl);
  });

  /**
   * True only when the nonce reaches this machine's probe; false only on proof the environment
   * was reached and nothing listens on the probe port there. Anything else is unknown.
   */
  const probeLoopback = Effect.fn("LoopbackTunnel.probeLoopback")(
    function* (environmentId: string, tunnelUrl: string) {
      const unavailable = new LoopbackTunnelUnavailableError({ environmentId });
      const nonce = new TextEncoder().encode(yield* random.uuidV4());
      const arrived = yield* Deferred.make<void>();
      const probe = yield* NodeSocketServer.make({ port: 0, host: "127.0.0.1" });
      yield* probe
        .run(
          Effect.fnUntraced(
            function* (socket: Socket.Socket) {
              const pull = yield* Socket.readerBytes(socket);
              let received = Buffer.alloc(0);
              while (received.length < nonce.length) {
                received = Buffer.concat([received, ...(yield* pull)]);
              }
              if (received.subarray(0, nonce.length).equals(nonce)) {
                yield* Deferred.succeed(arrived, undefined);
              }
            },
            Effect.scoped,
            (effect) => Effect.ignoreCause(effect),
          ),
        )
        .pipe(Effect.forkScoped);
      let portRefused = false;
      const destination = yield* dialTunnel(
        tunnelUrl,
        { host: "localhost", port: (probe.address as NetAddress.InetAddress).port },
        () => {
          portRefused = true;
        },
      ).pipe(Effect.option);
      if (Option.isNone(destination)) {
        if (portRefused) return false;
        return yield* unavailable;
      }
      yield* destination.value.writer.write(nonce);
      // An answer or a close means a service on another machine took the connection.
      const answer = yield* Effect.raceFirst(
        Deferred.await(arrived).pipe(Effect.as(true)),
        destination.value.pull.pipe(Effect.as(false)),
      ).pipe(Effect.timeoutOption(PROBE_TIMEOUT));
      if (Option.isNone(answer)) return yield* unavailable;
      return answer.value;
    },
    Effect.scoped,
    (effect, environmentId) =>
      effect.pipe(
        Effect.mapError((error) =>
          error._tag === "LoopbackTunnelUnavailableError"
            ? error
            : new LoopbackTunnelUnavailableError({ environmentId }),
        ),
      ),
  );

  const forwardKey = (environmentId: string, port: number) => `${environmentId}:${port}`;
  const currentForwards = () => [...forwarded.values()].map(({ forward }) => forward);
  const announceForwards = Effect.suspend(() => {
    const forwards = currentForwards();
    return Effect.forEach([...forwardListeners], (listener) => listener(forwards), {
      discard: true,
    });
  });

  // `localhost` may resolve to ::1 first, so a forward holds the port there too; a local server
  // on [::1] alone would otherwise answer the forward's `localhost` address.
  const bindLoopback = Effect.fn("LoopbackTunnel.bindLoopback")(function* (port: number) {
    const v4 = yield* NodeSocketServer.make({ port, host: "127.0.0.1" });
    const boundPort = (v4.address as NetAddress.InetAddress).port;
    const v6 = yield* NodeSocketServer.make({ port: boundPort, host: "::1" }).pipe(
      Effect.asSome,
      Effect.catchIf(
        // A host without IPv6 has no ::1 for anything to listen on.
        (error) =>
          ["EADDRNOTAVAIL", "EAFNOSUPPORT"].includes(
            (error.reason.cause as NodeJS.ErrnoException | undefined)?.code ?? "",
          ),
        () => Effect.succeedNone,
      ),
    );
    return { port: boundPort, servers: [v4, ...Option.toArray(v6)] };
  });

  const listenForward = Effect.fn("LoopbackTunnel.listenForward")(function* (
    environmentId: EnvironmentId,
    remotePort: number,
    stopsBefore: number,
  ) {
    // A wildcard server (:: or 0.0.0.0) does not collide with the specific loopback binds on
    // macOS and Windows, and would lose `localhost` to the forward, so the forward moves off its
    // port. Linux refuses those binds already, and its probes also collide with non-loopback
    // hosts. Probed before the Stop check, so a Stop during the probe is still seen.
    const servedOnWildcard =
      platform !== "linux" &&
      (!(yield* net.canListenOnHost(remotePort, "::")) ||
        !(yield* net.canListenOnHost(remotePort, "0.0.0.0")));
    const key = forwardKey(environmentId, remotePort);
    if ((forwardStops.get(key) ?? 0) !== stopsBefore) {
      return yield* new LoopbackTunnelForwardError({
        environmentId,
        port: remotePort,
        cause: new Error("The forward was stopped while it started."),
      });
    }
    const existing = forwarded.get(key);
    if (existing) return existing.forward;
    const scope = yield* Scope.fork(listenerScope);
    // Each attempt binds in its own scope, so a half-bound pair is released before the next.
    const bind = (port: number) =>
      Scope.fork(scope).pipe(
        Effect.flatMap((attempt) =>
          bindLoopback(port).pipe(
            // With SO_REUSEADDR, macOS can hand out the port the wildcard server holds.
            Effect.filterOrFail(
              (bound) => port !== 0 || bound.port !== remotePort,
              () =>
                new LoopbackTunnelForwardError({
                  environmentId,
                  port: remotePort,
                  cause: "The free port was the local server's.",
                }),
            ),
            Scope.provide(attempt),
            Effect.onError(() => Scope.close(attempt, Exit.void)),
          ),
        ),
      );
    const bindElsewhere = bind(0).pipe(Effect.retry({ times: 2 }));
    const bound = yield* (
      servedOnWildcard
        ? bindElsewhere
        : // Taken here, or privileged: any free port still reaches the environment's.
          bind(remotePort).pipe(Effect.catch(() => bindElsewhere))
    ).pipe(
      Effect.onError(() => Scope.close(scope, Exit.void)),
      Effect.mapError(
        (cause) => new LoopbackTunnelForwardError({ environmentId, port: remotePort, cause }),
      ),
    );
    const handler = serveForward(environmentId, remotePort);
    yield* Effect.forEach(
      bound.servers,
      (server) => server.run(handler).pipe(Effect.forkIn(scope)),
      { discard: true },
    );
    const forward = { environmentId, remotePort, localPort: bound.port };
    forwarded.set(key, { forward, scope });
    yield* announceForwards;
    return forward;
  }, forwarding.withPermits(1));

  const forward: LoopbackTunnel["Service"]["forward"] = Effect.fn("LoopbackTunnel.forward")(
    function* ({ environmentId, remotePort }) {
      const key = forwardKey(environmentId, remotePort);
      const existing = forwarded.get(key);
      if (existing) return existing.forward;
      const stopsBefore = forwardStops.get(key) ?? 0;
      // Outside the permit: the probe can take seconds, and Stop must not wait on it.
      if (yield* sharesLoopback(environmentId)) {
        return yield* new LoopbackTunnelSameMachineError({ environmentId, port: remotePort });
      }
      return yield* listenForward(environmentId, remotePort, stopsBefore);
    },
  );

  const closeForward = Effect.fn("LoopbackTunnel.closeForward")(function* (key: string) {
    const existing = forwarded.get(key);
    if (!existing) return;
    forwarded.delete(key);
    // Closing the scope also ends the connections it carries.
    yield* Scope.close(existing.scope, Exit.void);
    yield* announceForwards;
  }, forwarding.withPermits(1));

  const stopForward: LoopbackTunnel["Service"]["stopForward"] = Effect.fn(
    "LoopbackTunnel.stopForward",
  )(function* ({ environmentId, remotePort }) {
    const key = forwardKey(environmentId, remotePort);
    // Counted before the permit, so only starts already underway see this Stop.
    forwardStops.set(key, (forwardStops.get(key) ?? 0) + 1);
    yield* closeForward(key);
  });

  const subscribeForwards: LoopbackTunnel["Service"]["subscribeForwards"] = (listener) =>
    Effect.acquireRelease(
      Effect.sync(() => forwardListeners.add(listener)),
      () => Effect.sync(() => forwardListeners.delete(listener)),
    ).pipe(Effect.asVoid);

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
    session.webRequest.onBeforeRequest({ urls: TUNNEL_ESCAPE_URLS }, keepInTunnel);
    routed.add(session);
  }, routing.withPermits(1));

  const resolveAccess: LoopbackTunnel["Service"]["resolveAccess"] = ({ requestId, url }) => {
    const answer = pending.get(requestId);
    return answer ? Deferred.succeed(answer, url).pipe(Effect.asVoid) : Effect.void;
  };

  return LoopbackTunnel.of({
    route,
    resolveAccess,
    forward,
    stopForward,
    forwards: Effect.sync(currentForwards),
    sharesLoopback,
    subscribeForwards,
  });
});

export const layer = Layer.effect(LoopbackTunnel, make);
