import * as Schema from "effect/Schema";

import { concat, fromBase64, toBase64Url, utf8, utf8Decode } from "./bytes.ts";

/**
 * What the gateway would otherwise keep in a database: challenges, device credentials and grants
 * travel as AES-256-GCM tokens only it can open. The purpose is bound into the key and the
 * associated data, so a token of one kind never opens as another.
 */
export type SealPurpose = "challenge" | "credential" | "grant";

export const SealingKeys = Schema.Struct({
  /** The key new tokens are sealed with. Older keys stay listed until their tokens expire. */
  current: Schema.String.check(Schema.isPattern(/^[a-z0-9]{1,16}$/)),
  keys: Schema.Record(Schema.String, Schema.String),
});
export type SealingKeys = typeof SealingKeys.Type;

const VERSION = 1;
const IV_BYTES = 12;

export interface Sealer {
  readonly seal: (purpose: SealPurpose, value: unknown) => Promise<string>;
  /** Null when the token is malformed, was sealed for another purpose, or its key is gone. */
  readonly open: (purpose: SealPurpose, token: string) => Promise<unknown>;
}

export function makeSealer(config: SealingKeys): Sealer {
  const masters = new Map<string, Uint8Array<ArrayBuffer>>();
  for (const [id, encoded] of Object.entries(config.keys)) {
    const bytes = fromBase64(encoded);
    if (bytes === null || bytes.byteLength !== 32) {
      throw new Error(`Sealing key "${id}" must be 32 bytes of base64.`);
    }
    masters.set(id, bytes);
  }
  if (!masters.has(config.current)) {
    throw new Error(`Sealing key "${config.current}" is named current but not listed.`);
  }

  const derived = new Map<string, Promise<CryptoKey>>();
  const keyFor = (id: string, purpose: SealPurpose): Promise<CryptoKey> | null => {
    const master = masters.get(id);
    if (master === undefined) return null;
    const cacheKey = `${id}/${purpose}`;
    let key = derived.get(cacheKey);
    if (key === undefined) {
      key = crypto.subtle.importKey("raw", master, "HKDF", false, ["deriveKey"]).then((base) =>
        crypto.subtle.deriveKey(
          {
            name: "HKDF",
            hash: "SHA-256",
            salt: new Uint8Array(32),
            info: utf8(`onicode-push/${purpose}`),
          },
          base,
          { name: "AES-GCM", length: 256 },
          false,
          ["encrypt", "decrypt"],
        ),
      );
      derived.set(cacheKey, key);
    }
    return key;
  };

  return {
    seal: async (purpose, value) => {
      const id = utf8(config.current);
      const iv = crypto.getRandomValues(new Uint8Array(IV_BYTES));
      const header = concat(Uint8Array.of(VERSION, id.byteLength), id);
      const ciphertext = await crypto.subtle.encrypt(
        { name: "AES-GCM", iv, additionalData: concat(header, utf8(purpose)) },
        await keyFor(config.current, purpose)!,
        utf8(JSON.stringify(value)),
      );
      return toBase64Url(concat(header, iv, new Uint8Array(ciphertext)));
    },
    open: async (purpose, token) => {
      const bytes = fromBase64(token);
      if (bytes === null || bytes[0] !== VERSION) return null;
      const idLength = bytes[1];
      if (idLength === undefined || bytes.byteLength < 2 + idLength + IV_BYTES + 16) return null;
      const header = bytes.subarray(0, 2 + idLength);
      const id = utf8Decode(header.subarray(2));
      const key = id === null ? null : keyFor(id, purpose);
      if (key === null) return null;
      try {
        const plaintext = await crypto.subtle.decrypt(
          {
            name: "AES-GCM",
            iv: bytes.subarray(header.byteLength, header.byteLength + IV_BYTES),
            additionalData: concat(header, utf8(purpose)),
          },
          await key,
          bytes.subarray(header.byteLength + IV_BYTES),
        );
        return JSON.parse(utf8Decode(new Uint8Array(plaintext)) ?? "null") as unknown;
      } catch {
        return null;
      }
    },
  };
}
