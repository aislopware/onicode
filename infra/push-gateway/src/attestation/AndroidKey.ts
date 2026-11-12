import {
  AttestationApplicationId,
  NonStandardKeyMintKeyDescription,
  SecurityLevel,
  VerifiedBootState,
} from "@peculiar/asn1-android";
import { AsnConvert } from "@peculiar/asn1-schema";

import { equal, sha256, toHex, utf8Decode } from "../bytes.ts";
import * as DateTime from "effect/DateTime";

import { type AttestationResult, rejected } from "./result.ts";
import { X509Certificate } from "./x509.ts";

const KEY_DESCRIPTION_EXTENSION = "1.3.6.1.4.1.11129.2.1.17";
/** The subject attribute that names a factory-provisioned attestation key. */
const SERIAL_NUMBER_ATTRIBUTE = "2.5.4.5";
const P256_POINT_BYTES = 65;

/**
 * SHA-256 of the public keys Google signs attestation roots with, from
 * https://developer.android.com/privacy-and-security/security-key-attestation. Google reissued
 * the RSA root certificate several times over one key, so the key is what is pinned.
 */
const GOOGLE_ROOT_KEY_DIGESTS = new Set([
  "feb2ea7551ee316ed4bb443c8293b884dbfdea40b603ee3e4f4a897e4580fbae",
  "3ee44512a1af2beb39c889490c60ea3f82e43f5d5a5532f5ab9419f676cd07ec",
]);

/**
 * How far the device's boot chain must vouch for the OS. The app identity sits in the fields the
 * OS fills in, so only `verified` proves which app holds the key; `locked` also admits a locked
 * bootloader running a user-signed OS, and `any` admits unlocked devices.
 */
export type AndroidBootPolicy = "verified" | "locked" | "any";

export interface AndroidApp {
  readonly packageName: string;
  /** Lowercase hex SHA-256 digests of the certificates the APK may be signed with. */
  readonly signingCertificateDigests: ReadonlyArray<string>;
}

/** The ASN.1 library hands back its OctetString wrapper, a view, or a bare buffer by field. */
function bytesOf(
  value: ArrayBuffer | ArrayBufferView | { readonly buffer: ArrayBuffer },
): Uint8Array {
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  return ArrayBuffer.isView(value)
    ? new Uint8Array(value.buffer, value.byteOffset, value.byteLength)
    : new Uint8Array(value.buffer);
}

/** Serials as the revocation list keys them: lowercase hex without leading zeros. */
function revocationKey(serialNumber: string): string {
  return serialNumber.toLowerCase().replace(/^0+(?=.)/, "");
}

/** The steps of "Verify hardware-backed key pairs with key attestation". */
export async function verifyAndroidKeyAttestation(input: {
  /** DER certificates, the attested key first and the root last. */
  readonly chain: ReadonlyArray<Uint8Array<ArrayBuffer>>;
  readonly challengeHash: Uint8Array;
  readonly app: AndroidApp;
  readonly bootPolicy: AndroidBootPolicy;
  readonly revokedSerials: ReadonlySet<string>;
  readonly now: DateTime.Utc;
}): Promise<AttestationResult<{ readonly publicKey: Uint8Array<ArrayBuffer> }>> {
  let certificates: ReadonlyArray<X509Certificate>;
  try {
    certificates = input.chain.map((der) => new X509Certificate(der));
  } catch {
    return rejected("attestation-malformed");
  }
  const leaf = certificates[0];
  const root = certificates.at(-1);
  if (leaf === undefined || root === undefined || certificates.length < 2) {
    return rejected("attestation-malformed");
  }

  const rootKeyDigest = toHex(await sha256(new Uint8Array(root.publicKey.rawData)));
  if (!GOOGLE_ROOT_KEY_DIGESTS.has(rootKeyDigest)) return rejected("chain-untrusted");

  // Factory keys cannot be rotated, so their chains stay trusted past expiry; any other chain
  // must be in date. Google's verifier tells them apart by the certificate under the root.
  const underRoot = certificates.at(-2);
  const factoryProvisioned =
    underRoot !== undefined &&
    underRoot !== leaf &&
    underRoot.subjectName.getField(SERIAL_NUMBER_ATTRIBUTE).length > 0;
  const now = DateTime.toEpochMillis(input.now);
  for (const [index, certificate] of certificates.entries()) {
    const issuer = certificates[index + 1] ?? root;
    const signed = await certificate
      .verify({ publicKey: issuer.publicKey, signatureOnly: true })
      .catch(() => false);
    if (!signed) return rejected("chain-untrusted");
    // The leaf's dates are set on the device, and the root is pinned by its key.
    if (certificate !== leaf && certificate !== root) {
      if (now < certificate.notBefore.getTime()) return rejected("chain-untrusted");
      if (now > certificate.notAfter.getTime() && !factoryProvisioned) {
        return rejected("chain-untrusted");
      }
    }
    if (input.revokedSerials.has(revocationKey(certificate.serialNumber))) {
      return rejected("certificate-revoked");
    }
  }

  // A key that can sign may mint a child certificate naming any app. Only the description nearest
  // the root is trustworthy, so it has to be the leaf's and no other certificate may carry one.
  const described = certificates.filter(
    (certificate) => certificate.getExtension(KEY_DESCRIPTION_EXTENSION) !== null,
  );
  if (described.length !== 1 || described[0] !== leaf) return rejected("chain-untrusted");

  let description: NonStandardKeyMintKeyDescription;
  let applicationId: AttestationApplicationId | null;
  try {
    description = AsnConvert.parse(
      leaf.getExtension(KEY_DESCRIPTION_EXTENSION)!.value,
      NonStandardKeyMintKeyDescription,
    );
    const encoded =
      description.softwareEnforced.findProperty("attestationApplicationId") ??
      description.hardwareEnforced.findProperty("attestationApplicationId");
    applicationId =
      encoded === undefined ? null : AsnConvert.parse(bytesOf(encoded), AttestationApplicationId);
  } catch {
    return rejected("attestation-malformed");
  }

  if (!equal(bytesOf(description.attestationChallenge), input.challengeHash)) {
    return rejected("challenge-invalid");
  }
  if (
    description.attestationSecurityLevel === SecurityLevel.software ||
    description.keyMintSecurityLevel === SecurityLevel.software
  ) {
    return rejected("device-untrusted");
  }

  if (input.bootPolicy !== "any") {
    const rootOfTrust = description.hardwareEnforced.findProperty("rootOfTrust");
    const trusted =
      rootOfTrust !== undefined &&
      rootOfTrust.deviceLocked &&
      (rootOfTrust.verifiedBootState === VerifiedBootState.verified ||
        (input.bootPolicy === "locked" &&
          rootOfTrust.verifiedBootState === VerifiedBootState.selfSigned));
    if (!trusted) return rejected("device-untrusted");
  }

  if (applicationId === null) return rejected("app-mismatch");
  const packageMatches = applicationId.packageInfos.some(
    (info) => utf8Decode(bytesOf(info.packageName)) === input.app.packageName,
  );
  const signerMatches = applicationId.signatureDigests.some((digest) =>
    input.app.signingCertificateDigests.includes(toHex(bytesOf(digest))),
  );
  if (!packageMatches || !signerMatches) return rejected("app-mismatch");

  const spki = new Uint8Array(leaf.publicKey.rawData);
  const publicKey = spki.slice(spki.byteLength - P256_POINT_BYTES);
  const algorithm = leaf.publicKey.algorithm as { name?: string; namedCurve?: string };
  if (algorithm.name !== "ECDSA" || algorithm.namedCurve !== "P-256" || publicKey[0] !== 0x04) {
    return rejected("key-mismatch");
  }
  return { ok: true, value: { publicKey } };
}
