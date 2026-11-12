import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import * as SecureStore from "expo-secure-store";

import { writeFileAtomically } from "../../lib/atomic-file";
import { EMPTY_PUSH_REGISTRATION_STATE, type PushRegistrationState } from "./pushRegistration";

const CREDENTIAL_KEY = "onicode.push.credential";
const ENVIRONMENTS_FILE = "push-registration.json";
// Grants are refreshed from the background, possibly while the device is locked.
const SECURE_OPTIONS = { keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY };

const Grant = Schema.Struct({
  token: Schema.String,
  grant: Schema.String,
  issuedAt: Schema.Number,
  expiresAt: Schema.Number,
});

const StoredCredential = Schema.Struct({
  gatewayUrl: Schema.String,
  credential: Schema.String,
  expiresAt: Schema.Number,
  key: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("hardware"), keyId: Schema.String }),
    Schema.Struct({ kind: Schema.Literal("software") }),
  ]),
});

/** Sealed grants and environment keys are useless without the device key, so they live in a file. */
const StoredEnvironments = Schema.Struct({
  environments: Schema.Record(
    Schema.String,
    Schema.Struct({
      environmentKey: Schema.String,
      gatewayUrls: Schema.Array(Schema.String),
      grants: Schema.Struct({
        notification: Schema.optionalKey(Grant),
        liveActivityStart: Schema.optionalKey(Grant),
      }),
    }),
  ),
});

const decodeCredential = Schema.decodeUnknownOption(Schema.fromJsonString(StoredCredential));
const decodeEnvironments = Schema.decodeUnknownOption(Schema.fromJsonString(StoredEnvironments));
const encodeCredential = Schema.encodeEffect(Schema.fromJsonString(StoredCredential));
const encodeEnvironments = Schema.encodeEffect(Schema.fromJsonString(StoredEnvironments));

async function environmentsFile() {
  const { File, Paths } = await import("expo-file-system");
  return new File(Paths.document, ENVIRONMENTS_FILE);
}

export const loadPushRegistration: Effect.Effect<PushRegistrationState> = Effect.promise(
  async () => {
    const [credential, file] = await Promise.all([
      SecureStore.getItemAsync(CREDENTIAL_KEY, SECURE_OPTIONS).catch(() => null),
      environmentsFile(),
    ]);
    const environments = file.exists
      ? await file
          .text()
          .then(decodeEnvironments)
          .catch(() => null)
      : null;
    const stored = environments?._tag === "Some" ? environments.value : null;
    const decodedCredential = credential === null ? null : decodeCredential(credential);
    return {
      credential: decodedCredential?._tag === "Some" ? decodedCredential.value : null,
      environments: stored?.environments ?? EMPTY_PUSH_REGISTRATION_STATE.environments,
    };
  },
);

export const savePushRegistration = Effect.fn("savePushRegistration")(
  function* (state: PushRegistrationState) {
    const credential = state.credential === null ? null : yield* encodeCredential(state.credential);
    const environments = yield* encodeEnvironments({ environments: state.environments });
    yield* Effect.promise(async () => {
      await (credential === null
        ? SecureStore.deleteItemAsync(CREDENTIAL_KEY, SECURE_OPTIONS)
        : SecureStore.setItemAsync(CREDENTIAL_KEY, credential, SECURE_OPTIONS));
      await writeFileAtomically(await environmentsFile(), environments);
    });
  },
  Effect.catchCause((cause) => Effect.logWarning("Could not save the push registration.", cause)),
);
