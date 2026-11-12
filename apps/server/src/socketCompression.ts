import * as NodeHttpServerRequest from "@effect/platform-node/NodeHttpServerRequest";
import { SOCKET_COMPRESSION_QUERY_PARAM } from "@onicode/shared/socketCompression";
import type { HttpServerRequest } from "effect/http";

/**
 * Keeps permessage-deflate off the socket this request upgrades to. `ws` negotiates the
 * extension from this header at upgrade, so call it before `request.upgrade`.
 * Encoded video and JPEG do not shrink, and each 80 KB frame costs about a millisecond of zlib.
 */
export const declineSocketCompression = (request: HttpServerRequest.HttpServerRequest) => {
  delete NodeHttpServerRequest.toIncomingMessage(request).headers["sec-websocket-extensions"];
};

export const clientDeclinesSocketCompression = (url: URL) =>
  url.searchParams.get(SOCKET_COMPRESSION_QUERY_PARAM) === "off";
