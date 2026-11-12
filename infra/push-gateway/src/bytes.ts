const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });

export const utf8 = (value: string): Uint8Array<ArrayBuffer> => encoder.encode(value);

export function utf8Decode(bytes: Uint8Array): string | null {
  try {
    return decoder.decode(bytes);
  } catch {
    return null;
  }
}

export function concat(...parts: ReadonlyArray<Uint8Array>): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(parts.reduce((total, part) => total + part.byteLength, 0));
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.byteLength;
  }
  return out;
}

/** Compares without stopping at the first difference, so timing does not leak a prefix match. */
export function equal(a: Uint8Array, b: Uint8Array): boolean {
  if (a.byteLength !== b.byteLength) return false;
  let diff = 0;
  for (let index = 0; index < a.byteLength; index += 1) diff |= a[index]! ^ b[index]!;
  return diff === 0;
}

export function toBase64Url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

/** Decodes standard or URL-safe base64, padded or not. */
export function fromBase64(value: string): Uint8Array<ArrayBuffer> | null {
  try {
    const binary = atob(value.replaceAll("-", "+").replaceAll("_", "/"));
    const out = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) out[index] = binary.charCodeAt(index);
    return out;
  } catch {
    return null;
  }
}

export function toHex(bytes: Uint8Array): string {
  let out = "";
  for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
  return out;
}

export async function sha256(bytes: Uint8Array<ArrayBuffer>): Promise<Uint8Array<ArrayBuffer>> {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
}

/** X.509 and Android sign ECDSA as DER; WebCrypto verifies the fixed-width r‖s form. */
export function ecdsaDerToRaw(der: Uint8Array, size: number): Uint8Array<ArrayBuffer> | null {
  // SEQUENCE { INTEGER r, INTEGER s }, each at most size + 1 bytes, so one length byte each.
  if (der[0] !== 0x30) return null;
  let offset = 2;
  if (der[1]! & 0x80) offset = 2 + (der[1]! & 0x7f);
  const out = new Uint8Array(size * 2);
  for (const slot of [0, 1]) {
    if (der[offset] !== 0x02) return null;
    const length = der[offset + 1];
    if (length === undefined || length > size + 1) return null;
    let value = der.subarray(offset + 2, offset + 2 + length);
    if (value.byteLength !== length) return null;
    while (value.byteLength > size && value[0] === 0) value = value.subarray(1);
    if (value.byteLength > size) return null;
    out.set(value, slot * size + size - value.byteLength);
    offset += 2 + length;
  }
  return offset === der.byteLength ? out : null;
}
