import type {
  AuthClientMetadata,
  AuthClientMetadataDeviceType,
  AuthClientPresentationMetadata,
} from "@onicode/contracts";
import type * as HttpServerRequest from "effect/http/HttpServerRequest";
import * as NodeNet from "node:net";
import { normalizeHostname } from "@onicode/shared/hostClassification";
import * as Mac from "effect/crypto/Mac";
import * as Base64Url from "effect/encoding/Base64Url";
import * as Result from "effect/Result";

/**
 * The loopback address a server given `host` listens on, or undefined when it may listen beyond
 * this machine. Unset means 127.0.0.1. Only a loopback literal or `localhost` counts: Node listens
 * on every interface for an empty host, and any other name resolves through DNS.
 */
export function loopbackBindAddress(host: string | undefined): string | undefined {
  if (host === undefined) return "127.0.0.1";
  const address = normalizeHostname(host);
  if (address === "localhost") return "127.0.0.1";
  return (NodeNet.isIPv4(address) && address.startsWith("127.")) || address === "::1"
    ? address
    : undefined;
}

export const isRemoteReachableHost = (host: string | undefined): boolean =>
  loopbackBindAddress(host) === undefined;

export function base64UrlEncode(input: string | Uint8Array): string {
  return typeof input === "string"
    ? Base64Url.encode(new TextEncoder().encode(input))
    : Base64Url.encode(input);
}

export function base64UrlDecodeUtf8(input: string): string {
  return Result.getOrThrow(Base64Url.decodeString(input));
}

export function timingSafeEqualBase64Url(left: string, right: string): boolean {
  const leftBytes = Base64Url.decode(left);
  const rightBytes = Base64Url.decode(right);
  return (
    Result.isSuccess(leftBytes) &&
    Result.isSuccess(rightBytes) &&
    Mac.timingSafeEqual(leftBytes.success, rightBytes.success)
  );
}

function normalizeNonEmptyString(value: string | null | undefined): string | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function normalizeIpAddress(value: string | null | undefined): string | undefined {
  const normalized = normalizeNonEmptyString(value);
  if (!normalized) {
    return undefined;
  }
  return normalized.startsWith("::ffff:") ? normalized.slice("::ffff:".length) : normalized;
}

function inferDeviceType(userAgent: string | undefined): AuthClientMetadataDeviceType {
  if (!userAgent) {
    return "unknown";
  }

  const normalized = userAgent.toLowerCase();
  if (/bot|crawler|spider|slurp|curl|wget/.test(normalized)) {
    return "bot";
  }
  if (/ipad|tablet/.test(normalized)) {
    return "tablet";
  }
  if (/iphone|android.+mobile|mobile/.test(normalized)) {
    return "mobile";
  }
  return "desktop";
}

function inferOs(userAgent: string | undefined): string | undefined {
  if (!userAgent) {
    return undefined;
  }
  const normalized = userAgent.toLowerCase();
  if (/iphone|ipad|ipod/.test(normalized)) return "iOS";
  if (/android/.test(normalized)) return "Android";
  if (/mac os x|macintosh/.test(normalized)) return "macOS";
  if (/windows nt/.test(normalized)) return "Windows";
  if (/linux/.test(normalized)) return "Linux";
  return undefined;
}

function readRemoteAddressFromSource(source: unknown): string | undefined {
  if (!source || typeof source !== "object") {
    return undefined;
  }

  const candidate = source as {
    readonly remoteAddress?: string | null;
    readonly socket?: {
      readonly remoteAddress?: string | null;
    };
  };

  return normalizeIpAddress(candidate.socket?.remoteAddress ?? candidate.remoteAddress);
}

export function deriveAuthClientMetadata(input: {
  readonly request: HttpServerRequest.HttpServerRequest;
  readonly presented?: AuthClientPresentationMetadata;
}): AuthClientMetadata {
  const userAgent = normalizeNonEmptyString(input.request.headers["user-agent"]);
  const ipAddress = readRemoteAddressFromSource(input.request.source);
  const os = input.presented?.os ?? inferOs(userAgent);
  return {
    ...(input.presented?.label ? { label: input.presented.label } : {}),
    ...(ipAddress ? { ipAddress } : {}),
    ...(userAgent ? { userAgent } : {}),
    deviceType: input.presented?.deviceType ?? inferDeviceType(userAgent),
    ...(os ? { os } : {}),
  };
}
