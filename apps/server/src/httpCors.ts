import { ORCHESTRATION_PROTOCOL_HEADER } from "@onicode/contracts";

export const apiCorsAllowedMethods = ["GET", "POST", "OPTIONS"] as const;
export const apiCorsAllowedHeaders = [
  "authorization",
  "b3",
  "traceparent",
  "content-type",
  ORCHESTRATION_PROTOCOL_HEADER,
] as const;
