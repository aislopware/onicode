import type { PushGatewayAttestationRejectedError } from "@onicode/contracts/pushGateway";

export type AttestationRejection = PushGatewayAttestationRejectedError["reason"];

export type AttestationResult<A> =
  | { readonly ok: true; readonly value: A }
  | { readonly ok: false; readonly reason: AttestationRejection };

export const rejected = (reason: AttestationRejection) => ({ ok: false, reason }) as const;
