import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Ref from "effect/Ref";
import * as Schedule from "effect/Schedule";
import * as Schema from "effect/Schema";
import * as Scope from "effect/Scope";

import * as Electron from "electron";

const DESKTOP_HOST = "app";
const DESKTOP_PRODUCTION_SCHEME = "onicode";
const DESKTOP_DEVELOPMENT_SCHEME = "onicode-dev";

export function getDesktopScheme(isDevelopment: boolean): string {
  return isDevelopment ? DESKTOP_DEVELOPMENT_SCHEME : DESKTOP_PRODUCTION_SCHEME;
}

function getDesktopOrigin(isDevelopment: boolean): string {
  return `${getDesktopScheme(isDevelopment)}://${DESKTOP_HOST}`;
}

export function getDesktopUrl(isDevelopment: boolean): string {
  return `${getDesktopOrigin(isDevelopment)}/`;
}

export class ElectronProtocolRegistrationError extends Schema.TaggedError<ElectronProtocolRegistrationError>()(
  "ElectronProtocolRegistrationError",
  {
    scheme: Schema.String,
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Failed to register Electron protocol scheme "${this.scheme}".`;
  }
}

export class ElectronProtocolUnregistrationError extends Schema.TaggedError<ElectronProtocolUnregistrationError>()(
  "ElectronProtocolUnregistrationError",
  {
    scheme: Schema.String,
    cause: Schema.Defect(),
  },
) {
  override get message(): string {
    return `Failed to unregister Electron protocol scheme "${this.scheme}".`;
  }
}

export type DesktopProtocolRegistrationInput = {
  readonly scheme: string;
} & ({ readonly targetOrigin: URL } | { readonly assetDirectory: string });

export class ElectronProtocol extends Context.Service<
  ElectronProtocol,
  {
    readonly registerDesktopProtocol: (
      input: DesktopProtocolRegistrationInput,
    ) => Effect.Effect<void, ElectronProtocolRegistrationError, Scope.Scope>;
  }
>()("@onicode/desktop/electron/ElectronProtocol") {}

function makeDesktopContentSecurityPolicy(input: DesktopProtocolRegistrationInput): string {
  const scriptSources = ["'self'", "'unsafe-inline'", "'wasm-unsafe-eval'"];

  // User-configured environment origins are unknown here, so allow supported network schemes.
  // GLTFLoader fetches embedded textures through blob URLs.
  const connectSources = ["'self'", "blob:", "http:", "https:", "ws:", "wss:"];

  return [
    "default-src 'self'",
    `script-src ${scriptSources.join(" ")}`,
    `connect-src ${connectSources.join(" ")}`,
    `img-src 'self' ${input.scheme}: blob: data: http: https:`,
    `media-src 'self' ${input.scheme}: blob: http: https:`,
    "style-src 'self' 'unsafe-inline'",
    `font-src 'self' ${input.scheme}: data:`,
    "worker-src 'self' blob:",
    "frame-src 'self' blob: http: https:",
    "object-src 'self' blob: http: https:",
    "form-action 'self'",
  ].join("; ");
}

function withContentSecurityPolicy(response: Response, policy: string): Response {
  const headers = new Headers(response.headers);
  headers.set("Content-Security-Policy", policy);
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}

// Must run synchronously before Electron emits ready.
function registerDesktopSchemePrivilegesSync(): void {
  Electron.protocol.registerSchemesAsPrivileged([
    {
      scheme: DESKTOP_PRODUCTION_SCHEME,
      privileges: {
        standard: true,
        secure: true,
        supportFetchAPI: true,
        corsEnabled: true,
        stream: true,
        // Custom schemes must opt into V8 caching; dev URLs serve changing code.
        codeCache: true,
      },
    },
    {
      scheme: DESKTOP_DEVELOPMENT_SCHEME,
      privileges: {
        standard: true,
        secure: true,
        supportFetchAPI: true,
        corsEnabled: true,
        stream: true,
      },
    },
  ]);
}

const registerDesktopSchemePrivileges = Effect.sync(registerDesktopSchemePrivilegesSync).pipe(
  Effect.withSpan("desktop.electron.protocol.registerSchemePrivileges"),
);

export const layerSchemePrivileges = Layer.effectDiscard(registerDesktopSchemePrivileges);

class ElectronProtocolFetchError extends Schema.TaggedError<ElectronProtocolFetchError>()(
  "ElectronProtocolFetchError",
  { cause: Schema.Defect() },
) {}

const netFetch = (url: string, init: RequestInit) =>
  Effect.tryPromise({
    try: () => Electron.net.fetch(url, init),
    catch: (cause) => new ElectronProtocolFetchError({ cause }),
  });

const fetchWithTransientRetry = (url: string, init: RequestInit) =>
  netFetch(url, init).pipe(
    Effect.retry({ schedule: Schedule.exponential("50 millis", 3), times: 2 }),
  );

const proxyRequest = Effect.fn("desktop.protocol.proxyRequest")(function* (
  request: Request,
  targetOrigin: URL,
  contentSecurityPolicy: string,
) {
  const requestUrl = new URL(request.url);
  if (requestUrl.host !== DESKTOP_HOST) {
    return new Response(null, { status: 404 });
  }

  // Assign the path so leading slashes cannot replace the configured host.
  const targetUrl = new URL(targetOrigin);
  targetUrl.pathname = requestUrl.pathname;
  targetUrl.search = requestUrl.search;
  const headers = new Headers(request.headers);
  const headersToRemove: string[] = [];
  for (const name of headers.keys()) {
    if (
      name === "host" ||
      name === "origin" ||
      name === "referer" ||
      name === "connection" ||
      name === "content-length" ||
      name === "accept-encoding" ||
      name === "upgrade-insecure-requests" ||
      name.startsWith("sec-fetch-")
    ) {
      headersToRemove.push(name);
    }
  }
  for (const name of headersToRemove) {
    headers.delete(name);
  }
  const init: RequestInit = {
    method: request.method,
    headers,
  };
  if (request.method !== "GET" && request.method !== "HEAD") {
    init.body = request.body;
    (init as RequestInit & { duplex: "half" }).duplex = "half";
  }
  const response =
    request.method === "GET" || request.method === "HEAD"
      ? yield* fetchWithTransientRetry(targetUrl.toString(), init)
      : yield* netFetch(targetUrl.toString(), init);
  return withContentSecurityPolicy(response, contentSecurityPolicy);
});

/** @public */
export const make = Effect.gen(function* () {
  const registered = yield* Ref.make(false);
  const runPromise = Effect.runPromiseWith(yield* Effect.context<never>());

  const registerDesktopProtocol = Effect.fn("desktop.electron.protocol.registerDesktopProtocol")(
    function* (input: DesktopProtocolRegistrationInput) {
      if (yield* Ref.get(registered)) return;

      const contentSecurityPolicy = makeDesktopContentSecurityPolicy(input);

      yield* Effect.acquireRelease(
        Effect.try({
          try: () => {
            if ("assetDirectory" in input) {
              // Served by the network service, so asset loads never wait on a busy main thread.
              Electron.protocol.registerSource(input.scheme, {
                routes: [
                  {
                    match: { host: DESKTOP_HOST },
                    source: {
                      type: "directory",
                      root: input.assetDirectory,
                      headers: { "Content-Security-Policy": contentSecurityPolicy },
                    },
                  },
                ],
              });
              return;
            }
            Electron.protocol.handle(input.scheme, (request) =>
              runPromise(
                proxyRequest(request, input.targetOrigin, contentSecurityPolicy).pipe(
                  Effect.catchTags({
                    ElectronProtocolFetchError: (error) => Effect.die(error.cause),
                  }),
                ),
              ),
            );
          },
          catch: (cause) => new ElectronProtocolRegistrationError({ scheme: input.scheme, cause }),
        }).pipe(Effect.andThen(Ref.set(registered, true))),
        () =>
          Effect.try({
            try: () => Electron.protocol.unhandle(input.scheme),
            catch: (cause) =>
              new ElectronProtocolUnregistrationError({
                scheme: input.scheme,
                cause,
              }),
          }).pipe(Effect.andThen(Ref.set(registered, false)), Effect.orDie),
      );
    },
  );

  return ElectronProtocol.of({ registerDesktopProtocol });
});

export const layer = Layer.effect(ElectronProtocol, make);
