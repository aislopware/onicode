import * as Effect from "effect/Effect";
import { requireOptionalNativeModule } from "expo";

import {
  PushDeviceKeyError,
  type HardwareAttestation,
  type PushDeviceKeys,
} from "./pushRegistration";

interface OniDeviceAttestation {
  isHardwareAttestationSupported(): boolean;
  attestHardwareKey(challenge: string): Promise<HardwareAttestation>;
  signWithHardwareKey(keyId: string, payload: string): Promise<string>;
  createSoftwareKey(): Promise<string>;
  signWithSoftwareKey(payload: string): Promise<string>;
  deleteKeys(): Promise<void>;
}

const native = requireOptionalNativeModule<OniDeviceAttestation>("OniDeviceAttestation");

const call = <A>(operation: string, run: (module: OniDeviceAttestation) => Promise<A>) =>
  Effect.tryPromise({
    try: () => run(native!),
    catch: (cause) => new PushDeviceKeyError({ operation, cause }),
  });

/** Null on a build without the native module, where push is unavailable. */
export function nativeDeviceKeys(): PushDeviceKeys | null {
  if (native === null) return null;
  return {
    hardwareAttestationSupported: native.isHardwareAttestationSupported(),
    attestHardwareKey: (challenge) =>
      call("attest", (module) => module.attestHardwareKey(challenge)),
    signWithHardwareKey: (keyId, payload) =>
      call("sign", (module) => module.signWithHardwareKey(keyId, payload)),
    createSoftwareKey: call("be created", (module) => module.createSoftwareKey()),
    signWithSoftwareKey: (payload) => call("sign", (module) => module.signWithSoftwareKey(payload)),
  };
}
