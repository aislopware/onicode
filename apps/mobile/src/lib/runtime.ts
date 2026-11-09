import * as SecureRandom from "effect/crypto/SecureRandom";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as ManagedRuntime from "effect/ManagedRuntime";
import * as ExpoCrypto from "expo-crypto";

import { ManagedRelay } from "@onicode/client-runtime/relay";
import { layerRemoteHttpClient } from "@onicode/client-runtime/rpc";
import { RelayMobileClientId } from "@onicode/contracts/relay";

import * as NativeWebSocket from "../native/nativeWebSocket";
import * as Persistence from "../persistence/layer";
import { disposeOnFoundationReplace, type FoundationHotModule } from "./foundation-fast-refresh";
import { whenProtectedDataAvailable } from "./protectedData";

declare const module: { readonly hot?: FoundationHotModule } | undefined;

const layerHttpClient = layerRemoteHttpClient(fetch);

// Hermes has no WebCrypto, so Effect's own SecureRandom layer cannot run here.
const layerSecureRandom = Layer.succeed(
  SecureRandom.SecureRandom,
  SecureRandom.make(ExpoCrypto.getRandomBytes),
);

// client-runtime still requires the relay services; an invalid URL makes its client a no-op.
const layerDisabledRelay = ManagedRelay.layer({
  relayUrl: "http://relay.invalid",
  clientId: RelayMobileClientId,
}).pipe(
  Layer.provideMerge(
    Layer.succeed(
      ManagedRelay.ManagedRelayDpopSigner,
      ManagedRelay.ManagedRelayDpopSigner.of({
        thumbprint: Effect.fail(
          new ManagedRelay.ManagedRelayDpopKeyLoadError({
            keyStore: "expo-secure-store",
            cause: "Onicode has no relay.",
          }),
        ),
        createProof: Effect.fn("ManagedRelay.ManagedRelayDpopSigner.createProof")((input) =>
          Effect.fail(
            new ManagedRelay.ManagedRelayDpopProofCreationError({
              method: input.method,
              url: input.url,
              cause: "Onicode has no relay.",
            }),
          ),
        ),
      }),
    ),
  ),
);

type RuntimeLayerSource =
  | typeof layerSecureRandom
  | typeof layerDisabledRelay
  | typeof NativeWebSocket.layer
  | typeof layerHttpClient
  | typeof Persistence.layer;

const layerRuntime = Layer.mergeAll(
  layerSecureRandom,
  layerDisabledRelay,
  NativeWebSocket.layer,
).pipe(
  Layer.provideMerge(layerHttpClient),
  Layer.provideMerge(Persistence.layer),
  // These layers read the keychain and the database while they build, and a
  // failed build is kept for the life of the process.
  Layer.provide(Layer.effectDiscard(Effect.promise(whenProtectedDataAvailable))),
);

export const runtime: ManagedRuntime.ManagedRuntime<
  Layer.Success<RuntimeLayerSource>,
  Layer.Error<RuntimeLayerSource>
> = ManagedRuntime.make(layerRuntime);

export const layer: Layer.Layer<
  Layer.Success<RuntimeLayerSource>,
  Layer.Error<RuntimeLayerSource>
> = Layer.effectContext(runtime.contextEffect);

disposeOnFoundationReplace(typeof module === "undefined" ? undefined : module.hot, () =>
  runtime.dispose(),
);
