// @effect-diagnostics-next-line nodeBuiltinImport:off -- A synchronous Ed25519 signature keeps signing out of every delivery's requirements.
import * as NodeCrypto from "node:crypto";

import type { PushEnvironmentKey } from "@onicode/contracts/pushGateway";
import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";

import * as ServerSecretStore from "../auth/ServerSecretStore.ts";

const PRIVATE_KEY_SECRET = "push-ed25519-private-key";

/**
 * The key a push gateway binds this environment's grants to. A device's grant only works with
 * deliveries this key signed, so the key stays for the life of the environment.
 */
export class PushSigningKey extends Context.Service<
  PushSigningKey,
  {
    /** The raw public key, base64url. */
    readonly publicKey: PushEnvironmentKey;
    /** A base64url Ed25519 signature over the text's UTF-8 bytes. */
    readonly sign: (text: string) => string;
  }
>()("onicode/push/PushSigningKey") {}

const loadPrivateKey = Effect.fn("PushSigningKey.loadPrivateKey")(function* () {
  const secrets = yield* ServerSecretStore.ServerSecretStore;
  const stored = yield* secrets.get(PRIVATE_KEY_SECRET);
  if (Option.isSome(stored)) return stored.value;
  const generated = NodeCrypto.generateKeyPairSync("ed25519").privateKey.export({
    type: "pkcs8",
    format: "der",
  });
  return yield* secrets.create(PRIVATE_KEY_SECRET, generated).pipe(
    Effect.as<Uint8Array>(generated),
    // Another process created the key between the read and the write; use that one.
    Effect.catchIf(ServerSecretStore.isSecretAlreadyExistsError, () =>
      secrets.get(PRIVATE_KEY_SECRET).pipe(
        Effect.flatMap(
          Option.match({
            onSome: Effect.succeed,
            onNone: () => Effect.die(new Error("The push signing key vanished while loading.")),
          }),
        ),
      ),
    ),
  );
});

export const layer = Layer.effect(
  PushSigningKey,
  Effect.gen(function* () {
    const der = yield* loadPrivateKey();
    const privateKey = NodeCrypto.createPrivateKey({
      key: Buffer.from(der),
      type: "pkcs8",
      format: "der",
    });
    const { x } = NodeCrypto.createPublicKey(privateKey).export({ format: "jwk" });
    if (x === undefined)
      return yield* Effect.die(new Error("The push signing key is not Ed25519."));
    return PushSigningKey.of({
      publicKey: x,
      sign: (text) =>
        NodeCrypto.sign(null, Buffer.from(text, "utf8"), privateKey).toString("base64url"),
    });
  }),
);
