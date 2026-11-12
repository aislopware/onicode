import { decode, encode } from "cborg";
import * as DateTime from "effect/DateTime";
import { describe, expect, it } from "vite-plus/test";

import { fromBase64, sha256, utf8 } from "../bytes.ts";
import { verifyAndroidKeyAttestation } from "./AndroidKey.ts";
import { verifyAppAttestAssertion, verifyAppAttestation } from "./AppAttest.ts";
import {
  ANDROID_FACTORY_CHAIN,
  ANDROID_PROVISIONED_CHAIN,
  ANDROID_UNLOCKED_CHAIN,
  APP_ATTEST_DEVELOPMENT,
  APP_ATTEST_PRODUCTION,
} from "./attestationVectors.ts";
import { X509Certificate } from "./x509.ts";

const APP_ID = "V8H6LQ9448.io.uebelacker.AppAttestExample";

async function appAttestInput(vector: typeof APP_ATTEST_PRODUCTION) {
  return {
    attestation: fromBase64(vector.attestation)!,
    keyId: fromBase64(vector.keyId)!,
    challengeHash: await sha256(fromBase64(vector.challenge)!),
    appId: APP_ID,
    now: DateTime.makeUnsafe("2024-06-01T00:00:00Z"),
  };
}

describe("verifyAppAttestation", () => {
  it.each([
    [APP_ATTEST_DEVELOPMENT, "development"],
    [APP_ATTEST_PRODUCTION, "production"],
  ] as const)(
    "accepts an attestation and reports the $1 environment",
    async (vector, environment) => {
      const result = await verifyAppAttestation(await appAttestInput(vector));
      expect(result).toMatchObject({ ok: true, value: { environment } });
    },
  );

  it.each([
    ["another challenge", { challengeHash: new Uint8Array(32) }, "challenge-invalid"],
    ["another app", { appId: "V8H6LQ9448.io.example.Other" }, "app-mismatch"],
    ["another key id", { keyId: new Uint8Array(32) }, "key-mismatch"],
    ["an expired chain", { now: DateTime.makeUnsafe("2026-01-01T00:00:00Z") }, "chain-untrusted"],
  ] as const)("rejects %s", async (_label, override, reason) => {
    const input = await appAttestInput(APP_ATTEST_PRODUCTION);
    expect(await verifyAppAttestation({ ...input, ...override })).toEqual({ ok: false, reason });
  });

  it("rejects an attestation whose bytes were altered", async () => {
    const input = await appAttestInput(APP_ATTEST_PRODUCTION);
    const attestation = input.attestation.slice();
    attestation[attestation.byteLength - 40]! ^= 1;
    expect(await verifyAppAttestation({ ...input, attestation })).toMatchObject({ ok: false });
  });
});

describe("verifyAppAttestation with a hostile certificate", () => {
  it("rejects a certificate whose signature does not parse instead of throwing", async () => {
    const input = await appAttestInput(APP_ATTEST_PRODUCTION);
    const decoded = decode(input.attestation) as {
      readonly attStmt: { readonly x5c: ReadonlyArray<Uint8Array> };
    };
    const intermediate = decoded.attStmt.x5c[1]!;
    // The signature is the last element: a BIT STRING holding a SEQUENCE of two integers.
    const at = intermediate.findLastIndex(
      (byte, index) =>
        byte === 0x03 && intermediate[index + 2] === 0x00 && intermediate[index + 3] === 0x30,
    );
    intermediate[at + 3] = 0x31;

    expect(await verifyAppAttestation({ ...input, attestation: encode(decoded) })).toEqual({
      ok: false,
      reason: "chain-untrusted",
    });
  });
});

describe("verifyAppAttestAssertion", () => {
  const assertion = fromBase64(
    "omlzaWduYXR1cmVYRzBFAiBB8BGAwkmFCg1M5J0mOYEun0SUN1/lse79/7ypG9WiMQIhAIHvqj7eg59B1PMFX1CN4GMGlsgfFtdL30pHCf7G/dNRcWF1dGhlbnRpY2F0b3JEYXRhWCXKPdw7T3iujcFZbHVrHX0mDSMrNms5PzEbrFbQPRA6rEAAAAAB",
  )!;
  // The last 65 bytes of the vector's SPKI.
  const publicKey = fromBase64(
    "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEg69t2YzgcPTLUx8Zgu+rbcikeaEL8Ppb+HG0QTIulz8YUB9tgv1pDRruWk87nZC3our56pzIWaqXEbaWyamdzA==",
  )!.slice(-65);
  const clientData = utf8(
    '{"subject":"Lorem ipsum","message":"Lorem ipsum dolor sit amet, consectetur adipiscing elit."}',
  );

  it("accepts a signature over the signed data and nothing else", async () => {
    const input = { assertion, publicKey, clientData, appId: APP_ID };
    expect(await verifyAppAttestAssertion(input)).toBe(true);
    expect(await verifyAppAttestAssertion({ ...input, clientData: utf8("{}") })).toBe(false);
    expect(await verifyAppAttestAssertion({ ...input, appId: "V8H6LQ9448.other" })).toBe(false);
  });
});

function androidChain(pem: string) {
  return (pem.match(/-----BEGIN CERTIFICATE-----[\s\S]+?-----END CERTIFICATE-----/g) ?? []).map(
    (pem) => fromBase64(pem.replace(/-----[^-]+-----|\s/g, ""))!,
  );
}

describe("verifyAndroidKeyAttestation", () => {
  const input = {
    chain: androidChain(ANDROID_PROVISIONED_CHAIN),
    challengeHash: utf8("e8652ec2-c321-4a3c-b115-95b59cff6835"),
    app: {
      packageName: "com.google.android.attestation",
      signingCertificateDigests: [
        "103938ee4537e59e8ee792f654504fb8346fc6b346d0bbc4415fc339fcfc8ec1",
      ],
    },
    bootPolicy: "verified",
    revokedSerials: new Set<string>(),
    now: DateTime.makeUnsafe("2026-09-08T00:00:00Z"),
  } as const;

  it("accepts a locked, verified device attesting the expected app", async () => {
    const result = await verifyAndroidKeyAttestation(input);
    expect(result).toMatchObject({ ok: true });
    expect(result.ok && result.value.publicKey.byteLength).toBe(65);
  });

  it.each([
    ["another challenge", { challengeHash: utf8("other") }, "challenge-invalid"],
    ["another package", { app: { ...input.app, packageName: "dev.example" } }, "app-mismatch"],
    [
      "another signer",
      { app: { ...input.app, signingCertificateDigests: ["00"] } },
      "app-mismatch",
    ],
    [
      "an expired provisioned chain",
      { now: DateTime.makeUnsafe("2026-12-01T00:00:00Z") },
      "chain-untrusted",
    ],
    ["a chain without its root", { chain: input.chain.slice(0, -1) }, "chain-untrusted"],
  ] as const)("rejects %s", async (_label, override, reason) => {
    expect(await verifyAndroidKeyAttestation({ ...input, ...override })).toEqual({
      ok: false,
      reason,
    });
  });

  it("trusts a factory chain past its expiry, but not before its certificates are valid", async () => {
    const factory = {
      ...input,
      chain: androidChain(ANDROID_FACTORY_CHAIN),
      challengeHash: utf8("912895e0-3e19-4327-a0fb-91e2f3f46cdb"),
      bootPolicy: "any",
    } as const;
    // Factory keys cannot be rotated, so Google keeps them trusted unless revoked.
    expect(await verifyAndroidKeyAttestation(factory)).toMatchObject({ ok: true });
    expect(
      await verifyAndroidKeyAttestation({
        ...factory,
        now: DateTime.makeUnsafe("2016-01-01T00:00:00Z"),
      }),
    ).toEqual({ ok: false, reason: "chain-untrusted" });
  });

  it("rejects a chain with a revoked certificate", async () => {
    const serial = new X509Certificate(input.chain[1]!).serialNumber.toLowerCase();
    const result = await verifyAndroidKeyAttestation({
      ...input,
      revokedSerials: new Set([serial.replace(/^0+(?=.)/, "")]),
    });
    expect(result).toEqual({ ok: false, reason: "certificate-revoked" });
  });

  it("rejects an unlocked device unless the policy admits any boot state", async () => {
    const unlocked = {
      ...input,
      chain: androidChain(ANDROID_UNLOCKED_CHAIN),
      challengeHash: utf8("challenge"),
      now: DateTime.makeUnsafe("2024-09-20T00:00:00Z"),
      app: {
        packageName: "com.google.wireless.android.security.attestationverifier.collector",
        signingCertificateDigests: [] as Array<string>,
      },
    };
    expect(await verifyAndroidKeyAttestation(unlocked)).toEqual({
      ok: false,
      reason: "device-untrusted",
    });
    expect(await verifyAndroidKeyAttestation({ ...unlocked, bootPolicy: "any" })).toEqual({
      ok: false,
      reason: "app-mismatch",
    });
  });
});
