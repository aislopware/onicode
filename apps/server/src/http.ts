import * as Mime from "effect/http/Mime";
import {
  AuthOrchestrationOperateScope,
  AuthOrchestrationReadScope,
  EnvironmentHttpApi,
} from "@onicode/contracts";
import {
  injectMcpAppCookieBootstrap,
  injectMcpAppDocumentBootstrap,
  injectMcpAppThemeBootstrap,
  refreshMcpAppCsp,
} from "@onicode/shared/mcpApp";
import {
  FILE_PREVIEW_SCROLL_SCRIPT,
  findStoredHtmlRenderBootstrap,
} from "@onicode/shared/htmlRender";
import { hostPreviewMimeTypeFromExtension } from "@onicode/shared/filePreview";
import {
  DESKTOP_READINESS_CHALLENGE_MAX_LENGTH,
  DESKTOP_READINESS_CHALLENGE_PARAM,
  DESKTOP_READINESS_PROOF_PATH,
  desktopReadinessProof,
} from "@onicode/shared/desktopReadinessProof";
import { decodeOtlpTraceRecords } from "@onicode/shared/observability";
import * as ByteSize from "effect/ByteSize";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as Tracer from "effect/Tracer";
import { cast, pipe } from "effect/Function";
import {
  HttpClient,
  HttpClientResponse,
  HttpMiddleware,
  HttpRouter,
  HttpServerResponse,
  HttpServerRequest,
  HttpServerRespondable,
} from "effect/http";
import * as HttpIncomingMessage from "effect/http/HttpIncomingMessage";
import * as HttpApiBuilder from "effect/http-api/HttpApiBuilder";
import { OtlpTracer, OtlpSerialization } from "effect/observability";

import * as ServerConfig from "./config.ts";
import { ASSET_ROUTE_PREFIX, readMcpAppHtml, resolveAsset } from "./assets/AssetAccess.ts";
import { sourceControlMediaResponse } from "./assets/sourceControlMediaResponse.ts";
import { statMediaFile, streamMediaFile, type OpenMediaFile } from "./assets/MediaFile.ts";
import {
  ATTACHMENT_UPLOAD_ROUTE_PREFIX,
  storeAttachmentUpload,
  validateAttachmentUploadToken,
} from "./assets/AttachmentUpload.ts";
import * as BrowserTraceCollector from "./observability/BrowserTraceCollector.ts";
import * as EnvironmentAuth from "./auth/EnvironmentAuth.ts";
import {
  annotateEnvironmentRequest,
  failEnvironmentScopeRequired,
  failEnvironmentAuthInvalid,
  failEnvironmentInternal,
} from "./auth/http.ts";
import * as ServerEnvironment from "./environment/ServerEnvironment.ts";
import { WEBHOOK_ROUTE_PREFIX } from "./scheduledTasks/ScheduledTaskService.ts";
import * as TailnetAccess from "./tailnet/TailnetAccess.ts";
import { apiCorsAllowedHeaders, apiCorsAllowedMethods, MCP_HTTP_PATH } from "./httpCors.ts";
import { requestUrl } from "./requestUrl.ts";

const OTLP_TRACES_PROXY_PATH = "/api/observability/v1/traces";
const SVG_CONTENT_SECURITY_POLICY = "default-src 'none'; style-src 'unsafe-inline'; sandbox";
// Opaque origin blocks cookies, storage and API access while allowing scripts.
// Agent HTML can open without a click, so disallow blocking modals.
const HTML_CONTENT_SECURITY_POLICY =
  "sandbox allow-scripts allow-forms allow-popups allow-downloads";

const XML_DOCUMENT_MIME_TYPE_PATTERN = /(?:\/xml(?:$|-)|\+xml$)/i;
// Proxies may strip disposition; force document-capable downloads to octet-stream.
const DOWNLOAD_MIME_TYPE_PATTERN = /^[\w!#$&^.+-]+\/[\w!#$&^.+-]+$/;
const isSafeDownloadMimeType = (mimeType: string): boolean =>
  DOWNLOAD_MIME_TYPE_PATTERN.test(mimeType) &&
  !/^text\/html$/i.test(mimeType.trim()) &&
  !XML_DOCUMENT_MIME_TYPE_PATTERN.test(mimeType.trim());

// The served type and its sandbox policy come from this one answer, so they cannot disagree.
const inlineAssetMimeType = (filePath: string, claimed: string | undefined): string => {
  const claimedType = claimed?.split(";", 1)[0]?.trim().toLowerCase();
  if (claimedType !== undefined && DOWNLOAD_MIME_TYPE_PATTERN.test(claimedType)) return claimedType;
  return (
    hostPreviewMimeTypeFromExtension(/\.[^./\\]+$/.exec(filePath)?.[0] ?? "") ??
    Option.getOrElse(Mime.getType(filePath), () => "application/octet-stream")
  );
};

export function downloadContentDisposition(fileName?: string): string {
  if (fileName === undefined) {
    return "attachment";
  }
  // toWellFormed: encodeURIComponent throws URIError on unpaired surrogates.
  const sanitized = fileName.toWellFormed().replace(/[\p{Cc}"\\]/gu, "_");
  const asciiFallback = sanitized.replace(/[^\u0020-\u007e]/g, "_");
  const needsExtended = asciiFallback !== sanitized;
  const extendedName = encodeURIComponent(sanitized).replace(
    /['()*]/g,
    (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`,
  );
  return `attachment; filename="${asciiFallback}"${
    needsExtended ? `; filename*=UTF-8''${extendedName}` : ""
  }`;
}

export function assetResponseHeaders(
  filePath: string,
  options?: {
    readonly download?: boolean;
    readonly fileName?: string;
    readonly mimeType?: string;
  },
): Record<string, string> {
  const base = { "Cache-Control": "private, max-age=3600", "X-Content-Type-Options": "nosniff" };
  if (options?.download) {
    return {
      ...base,
      "Content-Disposition": downloadContentDisposition(options.fileName),
      "Content-Security-Policy": "default-src 'none'; sandbox",
      "Content-Type":
        options.mimeType !== undefined && isSafeDownloadMimeType(options.mimeType)
          ? options.mimeType
          : "application/octet-stream",
    };
  }
  const mimeType = inlineAssetMimeType(filePath, options?.mimeType);
  if (mimeType === "text/html") {
    return {
      ...base,
      "Content-Type": "text/html; charset=utf-8",
      "Content-Security-Policy": HTML_CONTENT_SECURITY_POLICY,
    };
  }
  return {
    ...base,
    "Content-Type": mimeType,
    // XML types render as documents, so they get the same policy as SVG.
    ...(XML_DOCUMENT_MIME_TYPE_PATTERN.test(mimeType)
      ? { "Content-Security-Policy": SVG_CONTENT_SECURITY_POLICY }
      : {}),
  };
}

function assetByteRange(header: string, size: bigint) {
  const match = /^bytes=(\d*)-(\d*)$/i.exec(header.trim());
  if (!match || (!match[1] && !match[2])) return null;
  const first = match[1] ? BigInt(match[1]) : null;
  const last = match[2] ? BigInt(match[2]) : null;
  if (first !== null && last !== null && last < first) return null;
  if (size === 0n || (first !== null && first >= size) || (first === null && last === 0n)) {
    return { _tag: "Unsatisfiable" as const };
  }
  const start = first ?? (last! >= size ? 0n : size - last!);
  const end = first === null || last === null || last >= size ? size - 1n : last;
  if (!Number.isSafeInteger(Number(start)) || !Number.isSafeInteger(Number(end))) {
    return { _tag: "Unsatisfiable" as const };
  }
  return {
    _tag: "Range" as const,
    offset: start,
    bytesToRead: end - start + 1n,
    contentRange: `bytes ${start}-${end}/${size}`,
  };
}

// A render's bootstrap follows its <head> tag, so it sits in the leading bytes.
const HTML_RENDER_BOOTSTRAP_SCAN_BYTES = 1024 * 1024;

export const assetFileResponse = Effect.fn("assetFileResponse")(function* (
  asset: {
    readonly path: string;
    readonly download?: boolean;
    readonly fileName?: string;
    readonly mimeType?: string;
    readonly file?: OpenMediaFile;
    readonly renderIntent?: "mcp-app" | "file-preview" | "html-render";
  },
  rangeHeader?: string,
  ifRangeHeader?: string,
  method: "GET" | "HEAD" = "GET",
) {
  const headers = assetResponseHeaders(asset.path, asset);
  if (asset.renderIntent === "file-preview") {
    // The signed URL stays the same while the workspace file changes, so a reload must revalidate.
    headers["Cache-Control"] = "private, no-cache";
  }
  if (
    asset.renderIntent === "mcp-app" &&
    !asset.download &&
    headers["Content-Type"] === "text/html; charset=utf-8"
  ) {
    const bytes = yield* readMcpAppHtml(asset.path);
    if (bytes === null) {
      return HttpServerResponse.text("MCP app is too large to preview.", { status: 413 });
    }
    const body = pipe(
      new TextDecoder().decode(bytes),
      refreshMcpAppCsp,
      injectMcpAppThemeBootstrap,
      injectMcpAppCookieBootstrap,
      // Applied last so it is the document's first script.
      injectMcpAppDocumentBootstrap,
      (html) => new TextEncoder().encode(html),
    );
    headers["Content-Length"] = String(body.byteLength);
    return method === "HEAD"
      ? HttpServerResponse.empty({ status: 200, headers })
      : HttpServerResponse.uint8Array(body, { headers });
  }
  if (
    asset.renderIntent === "file-preview" &&
    !asset.download &&
    headers["Content-Type"] === "text/html; charset=utf-8"
  ) {
    const bytes = yield* readMcpAppHtml(asset.path, asset.file);
    // A page too large to buffer is served as is, without scroll restoration.
    if (bytes !== null) {
      const script = new TextEncoder().encode(FILE_PREVIEW_SCROLL_SCRIPT);
      const body = new Uint8Array(bytes.byteLength + script.byteLength);
      body.set(bytes);
      body.set(script, bytes.byteLength);
      headers["Content-Length"] = String(body.byteLength);
      return method === "HEAD"
        ? HttpServerResponse.empty({ status: 200, headers })
        : HttpServerResponse.uint8Array(body, { headers });
    }
  }
  // Attachments are written once, so reading by path again cannot mix two versions.
  if (
    asset.renderIntent === "html-render" &&
    !asset.download &&
    headers["Content-Type"] === "text/html; charset=utf-8"
  ) {
    const fs = yield* FileSystem.FileSystem;
    const head = Buffer.concat(
      yield* Stream.runCollect(
        fs.stream(asset.path, { bytesToRead: HTML_RENDER_BOOTSTRAP_SCAN_BYTES }),
      ),
    );
    const stored = findStoredHtmlRenderBootstrap(head.toString("latin1"));
    if (stored !== undefined) {
      const whole = head.byteLength < HTML_RENDER_BOOTSTRAP_SCAN_BYTES;
      const size = whole ? head.byteLength : Number((yield* fs.stat(asset.path)).size);
      const current = Buffer.from(stored.current);
      headers["Content-Length"] = String(size - (stored.end - stored.start) + current.byteLength);
      if (method === "HEAD") return HttpServerResponse.empty({ status: 200, headers });
      const swapped = Stream.succeed(
        Buffer.concat([head.subarray(0, stored.start), current, head.subarray(stored.end)]),
      );
      const body = whole
        ? swapped
        : Stream.concat(swapped, fs.stream(asset.path, { offset: head.byteLength }));
      return HttpServerResponse.stream(body, { headers });
    }
  }
  const mediaFile = asset.file;
  const mediaInfo = mediaFile ? yield* statMediaFile(asset.path, mediaFile) : undefined;
  const isMedia = /^(?:audio|video)\//i.test(asset.mimeType ?? "");
  if (isMedia) {
    // Without file identity, validators cannot guarantee byte equality; attachments must also expire with their URL.
    headers["Cache-Control"] = "private, no-store";
  }
  let status = 200;
  let offset = 0n;
  let bytesToRead: bigint | undefined;
  {
    headers["Accept-Ranges"] = "bytes";
    // If-Range requires a matching validator. A full response is safe when we cannot validate it.
    if (method === "GET" && rangeHeader && ifRangeHeader === undefined) {
      const fs = yield* FileSystem.FileSystem;
      const info = mediaInfo ?? (yield* fs.stat(asset.path));
      const range = assetByteRange(rangeHeader, info.size);
      if (range?._tag === "Unsatisfiable") {
        return HttpServerResponse.empty({
          status: 416,
          headers: { ...headers, "Content-Range": `bytes */${info.size}` },
        });
      }
      if (range?._tag === "Range") {
        status = 206;
        offset = range.offset;
        bytesToRead = range.bytesToRead;
        headers["Content-Range"] = range.contentRange;
      }
    }
  }
  if (mediaFile && mediaInfo) {
    const size = bytesToRead ?? mediaInfo.size;
    headers["Content-Length"] = String(size);
    if (!isMedia) {
      headers["Last-Modified"] = mediaInfo.mtime.toUTCString();
      headers.ETag = `W/"${mediaInfo.size.toString(16)}-${mediaInfo.mtimeMs.toString(16)}"`;
    }
    if (method === "HEAD" || size === 0n) {
      return HttpServerResponse.empty({ status, headers });
    }
    const body = streamMediaFile(mediaFile, offset, size);
    if (!body) {
      return HttpServerResponse.text("File is too large to preview.", { status: 413 });
    }
    return HttpServerResponse.stream(body, {
      status,
      headers,
    });
  }
  return yield* HttpServerResponse.file(asset.path, { status, offset, bytesToRead, headers });
});

// The default body limit for API routes. Routes that take more set their own limit.
export const API_MAX_BODY_BYTES = 1024 * 1024;
export const layerApiBodyLimit = HttpRouter.middleware(
  Effect.provideService(HttpIncomingMessage.MaxBodySize, ByteSize.bytes(API_MAX_BODY_BYTES)),
).layer;

export const layerHttpCompression = HttpRouter.middleware(HttpMiddleware.compression(), {
  global: true,
});

const apiCors = HttpMiddleware.cors({
  allowedMethods: apiCorsAllowedMethods,
  allowedHeaders: apiCorsAllowedHeaders,
  maxAge: 600,
});
// Clients send bearer tokens, never cookies, so any origin may call. MCP is left out: it
// refuses every browser Origin with 403, and a CORS answer would only invite the request.
export const layerApiCors = HttpRouter.middleware(
  (httpEffect) => {
    const withCors = apiCors(httpEffect);
    return Effect.flatMap(HttpServerRequest.HttpServerRequest, (request) =>
      request.url.split("?", 1)[0] === MCP_HTTP_PATH ? httpEffect : withCors,
    );
  },
  { global: true },
);

const authenticateRawRouteWithScope =
  /* Request instrumentation already owns the span; keep this helper in that context. */
  Effect.fnUntraced(function* (
    scope: typeof AuthOrchestrationReadScope | typeof AuthOrchestrationOperateScope,
  ) {
    const request = yield* HttpServerRequest.HttpServerRequest;
    const serverAuth = yield* EnvironmentAuth.EnvironmentAuth;
    const session = yield* serverAuth.authenticateHttpRequest(request).pipe(
      Effect.catchIf(EnvironmentAuth.isServerAuthCredentialError, (error) =>
        failEnvironmentAuthInvalid(EnvironmentAuth.serverAuthCredentialReason(error)),
      ),
      Effect.catchIf(EnvironmentAuth.isServerAuthInternalError, (error) =>
        failEnvironmentInternal("internal_error", error),
      ),
    );
    if (!session.scopes.includes(scope)) {
      return yield* failEnvironmentScopeRequired(scope);
    }
  });

export const layerServerEnvironmentHttpApi = HttpApiBuilder.group(
  EnvironmentHttpApi,
  "metadata",
  Effect.fnUntraced(function* (handlers) {
    const serverEnvironment = yield* ServerEnvironment.ServerEnvironment;
    const tailnet = yield* TailnetAccess.TailnetAccess;
    return handlers.handle(
      "descriptor",
      Effect.fn("environment.metadata.descriptor")(function* (args) {
        yield* annotateEnvironmentRequest(args.endpoint.name);
        const descriptor = yield* serverEnvironment.getDescriptor;
        return (yield* tailnet.signInEnabled())
          ? { ...descriptor, capabilities: { ...descriptor.capabilities, tailnetSignIn: true } }
          : descriptor;
      }),
    );
  }),
);

// Answers only for a launch that was handed a readiness secret, so the desktop app can tell the
// backend it started from any other listener at the same address.
export const layerDesktopReadinessProofRoute = HttpRouter.add(
  "GET",
  DESKTOP_READINESS_PROOF_PATH,
  Effect.gen(function* () {
    const config = yield* ServerConfig.ServerConfig;
    const request = yield* HttpServerRequest.HttpServerRequest;
    const secret = config.desktopReadinessSecret;
    const challenge = Option.getOrUndefined(requestUrl(request))?.searchParams.get(
      DESKTOP_READINESS_CHALLENGE_PARAM,
    );
    if (
      secret === undefined ||
      !challenge ||
      challenge.length > DESKTOP_READINESS_CHALLENGE_MAX_LENGTH
    ) {
      return HttpServerResponse.empty({ status: 404 });
    }
    return HttpServerResponse.text(yield* desktopReadinessProof(secret, challenge));
  }),
);

class DecodeOtlpTraceRecordsError extends Schema.TaggedError<DecodeOtlpTraceRecordsError>()(
  "DecodeOtlpTraceRecordsError",
  { cause: Schema.Defect() },
) {}

// Tracing once-per-second renderer exports would create more spans than it forwards.
export const layerOtlpTracesProxyRoute = HttpRouter.add(
  "POST",
  OTLP_TRACES_PROXY_PATH,
  Effect.gen(function* () {
    yield* authenticateRawRouteWithScope(AuthOrchestrationOperateScope);
    const request = yield* HttpServerRequest.HttpServerRequest;
    const config = yield* ServerConfig.ServerConfig;
    const otlpTracesUrl = config.otlpTracesUrl;
    const otlpHeaders = config.otlpTracesExport.headers;
    const browserTraceCollector = yield* BrowserTraceCollector.BrowserTraceCollector;
    const httpClient = yield* HttpClient.HttpClient;
    const serialization = yield* OtlpSerialization.OtlpSerialization;
    const bodyJson = cast<unknown, OtlpTracer.TraceData>(yield* request.json);

    yield* Effect.try({
      try: () => decodeOtlpTraceRecords(bodyJson),
      catch: (cause) => new DecodeOtlpTraceRecordsError({ cause }),
    }).pipe(
      Effect.flatMap((records) => browserTraceCollector.record(records)),
      Effect.catch((cause) => Effect.logWarning("Failed to decode browser OTLP traces", { cause })),
    );

    if (otlpTracesUrl === undefined) {
      return HttpServerResponse.empty({ status: 204 });
    }

    return yield* httpClient
      .post(otlpTracesUrl, {
        body: serialization.traces(bodyJson),
        headers: otlpHeaders,
      })
      .pipe(
        Effect.flatMap(HttpClientResponse.filterStatusOk),
        Effect.as(HttpServerResponse.empty({ status: 204 })),
        Effect.tapError((cause) =>
          Effect.logWarning("Failed to export browser OTLP traces", {
            cause,
            otlpTracesUrl,
          }),
        ),
        Effect.orElseSucceed(() =>
          HttpServerResponse.text("Trace export failed.", { status: 502 }),
        ),
      );
  }).pipe(
    Effect.catchTags({
      EnvironmentAuthInvalidError: HttpServerRespondable.toResponse,
      EnvironmentInternalError: HttpServerRespondable.toResponse,
      EnvironmentScopeRequiredError: HttpServerRespondable.toResponse,
    }),
    Effect.withTracerEnabled(false),
  ),
);

const requestPath = (request: HttpServerRequest.HttpServerRequest) => {
  const queryIndex = request.url.indexOf("?");
  return queryIndex === -1 ? request.url : request.url.slice(0, queryIndex);
};

// The path the router matches, found its way (FindMyWay `find`): drop an absolute target's origin,
// end at `?`, `;` or `#`, decode, collapse slashes, lower case. A signed route reached by any
// other spelling would put its token in spans and logs.
const routedPath = (url: string) => {
  const target = url.startsWith("/") ? url : url.replace(/^https?:\/\/.*?\//, "/");
  const end = target.search(/[?;#]/);
  const path = end === -1 ? target : target.slice(0, end);
  let decoded: string;
  try {
    decoded = decodeURI(path);
  } catch {
    decoded = path.replace(/%[0-7][0-9a-f]/gi, (escape) =>
      String.fromCharCode(parseInt(escape.slice(1), 16)),
    );
  }
  return decoded.replace(/\/{2,}/g, "/").toLowerCase();
};

const underPrefix = (path: string, prefix: string) =>
  path === prefix || path.startsWith(`${prefix}/`);

// Signed URLs carry bearer tokens in the path, which URL param redaction cannot reach.
const isSignedUrlRequest = (request: HttpServerRequest.HttpServerRequest) => {
  const path = routedPath(request.url);
  return (
    underPrefix(path, WEBHOOK_ROUTE_PREFIX) ||
    underPrefix(path, ASSET_ROUTE_PREFIX) ||
    underPrefix(path, ATTACHMENT_UPLOAD_ROUTE_PREFIX)
  );
};

/** Requests whose URL must never reach a span: the trace proxy itself and signed URLs. */
export const isUntracedRequest = (request: HttpServerRequest.HttpServerRequest) =>
  requestPath(request) === OTLP_TRACES_PROXY_PATH || isSignedUrlRequest(request);

// HttpRouter.serve builds routes privately; provide the server-wide TracerDisabledWhen to the served layer.
const layerUntracedRequests = Layer.succeed(HttpMiddleware.TracerDisabledWhen)(isUntracedRequest);

export const layerUnloggedSignedRequests = HttpRouter.middleware(
  (httpEffect) =>
    Effect.flatMap(HttpServerRequest.HttpServerRequest, (request) =>
      isSignedUrlRequest(request) ? HttpMiddleware.withLoggerDisabled(httpEffect) : httpEffect,
    ),
  { global: true },
);

export const withUntracedRequests = Layer.provide(layerUntracedRequests);

// Request spans otherwise parent to the server's lifetime span, so every request shares one trace id.
export const withRootRequestSpans = <A, E, R>(layer: Layer.Layer<A, E, R>) =>
  Layer.fromBuild((memoMap, scope) =>
    Layer.buildWithMemoMap(layer, memoMap, scope).pipe(
      // ParentSpan is optional to every reader, so removing it leaves R satisfied.
      Effect.updateContext(
        (context: Context.Context<R>) =>
          Context.omit(Tracer.ParentSpan)(context) as Context.Context<R>,
      ),
    ),
  );

export const layerAssetRoute = HttpRouter.add(
  "GET",
  `${ASSET_ROUTE_PREFIX}/*`,
  Effect.gen(function* () {
    const request = yield* HttpServerRequest.HttpServerRequest;
    const url = requestUrl(request);
    if (Option.isNone(url)) {
      return HttpServerResponse.text("Bad Request", { status: 400 });
    }

    // The router also hands this route `/api/assets;…`, which carries no asset.
    if (!url.value.pathname.startsWith(`${ASSET_ROUTE_PREFIX}/`)) {
      return HttpServerResponse.text("Not Found", { status: 404 });
    }
    const suffix = url.value.pathname.slice(`${ASSET_ROUTE_PREFIX}/`.length);
    const separatorIndex = suffix.indexOf("/");
    if (separatorIndex <= 0) {
      return HttpServerResponse.text("Not Found", { status: 404 });
    }

    const asset = yield* resolveAsset(
      suffix.slice(0, separatorIndex),
      suffix.slice(separatorIndex + 1),
    );
    if (!asset) {
      return HttpServerResponse.text("Not Found", { status: 404 });
    }
    if (asset.kind === "bytes") {
      return HttpServerResponse.uint8Array(asset.bytes, {
        contentType: asset.mimeType,
        headers: { "cache-control": "private, max-age=3600", "x-content-type-options": "nosniff" },
      });
    }
    if (asset.kind === "source-control-media") {
      return yield* sourceControlMediaResponse(asset, request.headers).pipe(
        Effect.tapError((cause) =>
          // The URL can carry a token in its query string, and the error keeps the request it
          // failed on; the host and the failure kind are enough to diagnose.
          Effect.logWarning("Failed to fetch source control media.", {
            host: URL.canParse(asset.url) ? new URL(asset.url).host : undefined,
            reason: cause.reason._tag,
          }),
        ),
        Effect.orElseSucceed(() =>
          HttpServerResponse.empty({
            status: 502,
            headers: { "cache-control": "private, no-store", "x-content-type-options": "nosniff" },
          }),
        ),
      );
    }
    return yield* assetFileResponse(
      asset,
      request.method === "GET" ? request.headers.range : undefined,
      request.headers["if-range"],
      request.method === "HEAD" ? "HEAD" : "GET",
    ).pipe(
      Effect.orElseSucceed(() => HttpServerResponse.text("Internal Server Error", { status: 500 })),
    );
  }),
);

export const layerAttachmentUploadRoute = HttpRouter.add(
  "POST",
  `${ATTACHMENT_UPLOAD_ROUTE_PREFIX}/*`,
  Effect.gen(function* () {
    const request = yield* HttpServerRequest.HttpServerRequest;
    const url = requestUrl(request);
    if (Option.isNone(url)) {
      return HttpServerResponse.text("Bad Request", { status: 400 });
    }

    const token = url.value.pathname.startsWith(`${ATTACHMENT_UPLOAD_ROUTE_PREFIX}/`)
      ? url.value.pathname.slice(`${ATTACHMENT_UPLOAD_ROUTE_PREFIX}/`.length)
      : "";
    if (!token) {
      return HttpServerResponse.text("Not Found", { status: 404 });
    }
    const claims = yield* validateAttachmentUploadToken(token);
    if (!claims) {
      return HttpServerResponse.text("Not Found", { status: 404 });
    }

    const contentLengthHeader = request.headers["content-length"];
    if (
      contentLengthHeader !== undefined &&
      (!Number.isInteger(Number(contentLengthHeader)) ||
        Number(contentLengthHeader) !== claims.sizeBytes)
    ) {
      return HttpServerResponse.text("Content-Length must match the upload size.", {
        status: 400,
      });
    }

    // Keep the request stream in the route scope until the response is sent.
    const bodyPull = yield* Stream.toPull(request.stream);
    const stored = yield* storeAttachmentUpload(claims, Stream.fromPull(Effect.succeed(bodyPull)));
    return stored.ok
      ? HttpServerResponse.empty({ status: 204 })
      : HttpServerResponse.text(stored.detail, { status: stored.status });
  }),
);
