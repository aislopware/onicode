import { ORCHESTRATION_PROTOCOL_HEADER } from "@onicode/contracts";

export const apiCorsAllowedMethods = ["GET", "POST", "OPTIONS"] as const;
export const apiCorsAllowedHeaders = [
  "authorization",
  "b3",
  "traceparent",
  "content-type",
  ORCHESTRATION_PROTOCOL_HEADER,
] as const;

/** MCP refuses every browser Origin with 403, so the CORS layer must not answer for it. */
export const MCP_HTTP_PATH = "/mcp";
