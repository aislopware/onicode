import { decode, decodeFirst } from "cborg";

import { concat, ecdsaDerToRaw, equal, sha256, utf8 } from "../bytes.ts";
import * as DateTime from "effect/DateTime";

import { type AttestationResult, rejected } from "./result.ts";
import { X509Certificate } from "./x509.ts";

/** https://www.apple.com/certificateauthority/Apple_App_Attestation_Root_CA.pem */
const APPLE_APP_ATTESTATION_ROOT_CA = `-----BEGIN CERTIFICATE-----
MIICITCCAaegAwIBAgIQC/O+DvHN0uD7jG5yH2IXmDAKBggqhkjOPQQDAzBSMSYw
JAYDVQQDDB1BcHBsZSBBcHAgQXR0ZXN0YXRpb24gUm9vdCBDQTETMBEGA1UECgwK
QXBwbGUgSW5jLjETMBEGA1UECAwKQ2FsaWZvcm5pYTAeFw0yMDAzMTgxODMyNTNa
Fw00NTAzMTUwMDAwMDBaMFIxJjAkBgNVBAMMHUFwcGxlIEFwcCBBdHRlc3RhdGlv
biBSb290IENBMRMwEQYDVQQKDApBcHBsZSBJbmMuMRMwEQYDVQQIDApDYWxpZm9y
bmlhMHYwEAYHKoZIzj0CAQYFK4EEACIDYgAERTHhmLW07ATaFQIEVwTtT4dyctdh
NbJhFs/Ii2FdCgAHGbpphY3+d8qjuDngIN3WVhQUBHAoMeQ/cLiP1sOUtgjqK9au
Yen1mMEvRq9Sk3Jm5X8U62H+xTD3FE9TgS41o0IwQDAPBgNVHRMBAf8EBTADAQH/
MB0GA1UdDgQWBBSskRBTM72+aEH/pwyp5frq5eWKoTAOBgNVHQ8BAf8EBAMCAQYw
CgYIKoZIzj0EAwMDaAAwZQIwQgFGnByvsiVbpTKwSga0kP0e8EeDS4+sQmTvb7vn
53O5+FRXgeLhpJ06ysC5PrOyAjEAp5U4xDgEgllF7En3VcE3iexZZtKeYnpqtijV
oyFraWVIyd/dganmrduC1bmTBGwD
-----END CERTIFICATE-----`;

const NONCE_EXTENSION = "1.2.840.113635.100.8.2";
/** SEQUENCE { [1] EXPLICIT OCTET STRING (32 bytes) }. */
const NONCE_EXTENSION_PREFIX = Uint8Array.of(0x30, 0x24, 0xa1, 0x22, 0x04, 0x20);
const P256_POINT_BYTES = 65;

// Apple's pages disagree on the sandbox name; real development attestations carry the first.
const DEVELOPMENT_AAGUIDS = ["appattestdevelop", "appattestsandbox"].map(utf8);
const PRODUCTION_AAGUID = concat(utf8("appattest"), new Uint8Array(7));

/** validationCategory values Apple lists as invalid or not meant for App Attest. */
const REFUSED_VALIDATION_CATEGORIES = new Set([0, 7, 8, 9]);

export type AppAttestEnvironment = "development" | "production";

export interface AppAttestKey {
  /** The attested P-256 key as an uncompressed point. */
  readonly publicKey: Uint8Array<ArrayBuffer>;
  readonly environment: AppAttestEnvironment;
}

interface AuthenticatorData {
  readonly rpIdHash: Uint8Array;
  readonly signCount: number;
  readonly attested: {
    readonly aaguid: Uint8Array;
    readonly credentialId: Uint8Array;
    readonly rest: Uint8Array;
  } | null;
}

function parseAuthenticatorData(data: Uint8Array, attested: boolean): AuthenticatorData | null {
  if (data.byteLength < 37) return null;
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const base = { rpIdHash: data.subarray(0, 32), signCount: view.getUint32(33) };
  if (!attested) return { ...base, attested: null };
  if (data.byteLength < 55) return null;
  const credentialIdLength = view.getUint16(53);
  const credentialEnd = 55 + credentialIdLength;
  if (data.byteLength < credentialEnd) return null;
  return {
    ...base,
    attested: {
      aaguid: data.subarray(37, 53),
      credentialId: data.subarray(55, credentialEnd),
      rest: data.subarray(credentialEnd),
    },
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** The extensions follow the COSE key; iOS versions before them stop at the key. */
function validationCategory(afterCredentialId: Uint8Array): number | null {
  try {
    const [, afterKey] = decodeFirst(afterCredentialId, { useMaps: true });
    if (afterKey.byteLength === 0) return null;
    const extensions: unknown = decode(afterKey, { useMaps: true });
    if (!(extensions instanceof Map)) return null;
    const category: unknown = extensions.get("apple_validation_category_01");
    if (typeof category === "number") return category;
    if (category instanceof Uint8Array && category.byteLength === 4) {
      return new DataView(category.buffer, category.byteOffset, 4).getUint32(0, true);
    }
    return null;
  } catch {
    return null;
  }
}

let root: X509Certificate | undefined;

/**
 * Apple's attestation checks from "Validating apps that connect to your server". The receipt is not
 * kept: nothing here asks Apple for a fraud risk metric.
 */
export async function verifyAppAttestation(input: {
  readonly attestation: Uint8Array;
  readonly keyId: Uint8Array;
  readonly challengeHash: Uint8Array;
  /** Team identifier, a period, and the bundle identifier. */
  readonly appId: string;
  readonly now: DateTime.Utc;
}): Promise<AttestationResult<AppAttestKey>> {
  let object: unknown;
  try {
    object = decode(input.attestation);
  } catch {
    return rejected("attestation-malformed");
  }
  if (!isRecord(object) || object.fmt !== "apple-appattest" || !isRecord(object.attStmt)) {
    return rejected("attestation-malformed");
  }
  const { x5c } = object.attStmt;
  const authData = object.authData;
  if (
    !Array.isArray(x5c) ||
    x5c.length !== 2 ||
    !x5c.every((entry) => entry instanceof Uint8Array) ||
    !(authData instanceof Uint8Array)
  ) {
    return rejected("attestation-malformed");
  }

  let leaf: X509Certificate;
  let intermediate: X509Certificate;
  try {
    leaf = new X509Certificate(x5c[0] as Uint8Array<ArrayBuffer>);
    intermediate = new X509Certificate(x5c[1] as Uint8Array<ArrayBuffer>);
    root ??= new X509Certificate(APPLE_APP_ATTESTATION_ROOT_CA);
  } catch {
    return rejected("attestation-malformed");
  }
  const date = DateTime.toDateUtc(input.now);
  // verify() throws on a signature it cannot parse.
  const chained =
    (await intermediate.verify({ publicKey: root.publicKey, date }).catch(() => false)) &&
    (await leaf.verify({ publicKey: intermediate.publicKey, date }).catch(() => false));
  if (!chained) return rejected("chain-untrusted");

  const nonce = await sha256(concat(authData, input.challengeHash));
  const extension = leaf.getExtension(NONCE_EXTENSION);
  const extensionValue = extension === null ? null : new Uint8Array(extension.value);
  if (
    extensionValue === null ||
    extensionValue.byteLength !== NONCE_EXTENSION_PREFIX.byteLength + 32 ||
    !equal(extensionValue.subarray(0, NONCE_EXTENSION_PREFIX.byteLength), NONCE_EXTENSION_PREFIX) ||
    !equal(extensionValue.subarray(NONCE_EXTENSION_PREFIX.byteLength), nonce)
  ) {
    return rejected("challenge-invalid");
  }

  const spki = new Uint8Array(leaf.publicKey.rawData);
  const publicKey = spki.slice(spki.byteLength - P256_POINT_BYTES);
  if (publicKey[0] !== 0x04 || !equal(await sha256(publicKey), input.keyId)) {
    return rejected("key-mismatch");
  }

  const parsed = parseAuthenticatorData(authData, true);
  if (parsed === null || parsed.attested === null) return rejected("attestation-malformed");
  if (!equal(parsed.rpIdHash, await sha256(utf8(input.appId)))) return rejected("app-mismatch");
  if (parsed.signCount !== 0 || !equal(parsed.attested.credentialId, input.keyId)) {
    return rejected("key-mismatch");
  }
  const { aaguid } = parsed.attested;
  const environment: AppAttestEnvironment | null = equal(aaguid, PRODUCTION_AAGUID)
    ? "production"
    : DEVELOPMENT_AAGUIDS.some((candidate) => equal(aaguid, candidate))
      ? "development"
      : null;
  if (environment === null) return rejected("environment-mismatch");

  const category = validationCategory(parsed.attested.rest);
  if (category !== null && REFUSED_VALIDATION_CATEGORIES.has(category)) {
    return rejected("device-untrusted");
  }
  return { ok: true, value: { publicKey, environment } };
}

/**
 * Checks an assertion over `clientData`. The gateway keeps no per-key counter, so freshness comes
 * from the timestamp inside the signed data, not from Apple's counter.
 */
export async function verifyAppAttestAssertion(input: {
  readonly assertion: Uint8Array;
  readonly publicKey: Uint8Array<ArrayBuffer>;
  readonly clientData: Uint8Array<ArrayBuffer>;
  readonly appId: string;
}): Promise<boolean> {
  let object: unknown;
  try {
    object = decode(input.assertion);
  } catch {
    return false;
  }
  if (!isRecord(object)) return false;
  const { signature, authenticatorData } = object;
  if (!(signature instanceof Uint8Array) || !(authenticatorData instanceof Uint8Array)) {
    return false;
  }
  const parsed = parseAuthenticatorData(authenticatorData, false);
  if (parsed === null || !equal(parsed.rpIdHash, await sha256(utf8(input.appId)))) return false;
  const rawSignature = ecdsaDerToRaw(signature, 32);
  if (rawSignature === null) return false;
  const nonce = await sha256(concat(authenticatorData, await sha256(input.clientData)));
  try {
    const key = await crypto.subtle.importKey(
      "raw",
      input.publicKey,
      { name: "ECDSA", namedCurve: "P-256" },
      false,
      ["verify"],
    );
    return await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, key, rawSignature, nonce);
  } catch {
    return false;
  }
}
