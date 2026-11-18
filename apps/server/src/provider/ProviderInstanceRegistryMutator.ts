/** Reconcile closes removed/replaced scopes before creating replacements: at most one live instance per id. */
import type {
  ProviderInstanceConfigMap,
  ProviderInstanceId,
  SecretReference,
} from "@onicode/contracts";
import * as Context from "effect/Context";
import type * as Effect from "effect/Effect";

export interface ProviderInstanceRegistryMutatorShape {
  readonly reconcile: (configMap: ProviderInstanceConfigMap) => Effect.Effect<void>;
  /** Re-reads the secrets of every instance, or one, and rebuilds those whose secrets changed. */
  readonly reloadSecrets: (instanceId?: ProviderInstanceId) => Effect.Effect<void>;
  /**
   * Rebuilds the instances `select` names from the host environment, once per change of the host
   * PATH since instances were built on it. Drivers copy the environment when built.
   */
  readonly rebuildOnHostPathChange: (
    select: Effect.Effect<ReadonlyArray<ProviderInstanceId>>,
  ) => Effect.Effect<void>;
  /**
   * Rebuilds the instances that read `reference` with `value`, keeping the other secrets a live
   * instance holds, and returns them. A parked instance reads its other secrets from the resolver.
   */
  readonly applySecret: (
    reference: SecretReference,
    value: string,
  ) => Effect.Effect<ReadonlyArray<ProviderInstanceId>>;
}

export class ProviderInstanceRegistryMutator extends Context.Service<
  ProviderInstanceRegistryMutator,
  ProviderInstanceRegistryMutatorShape
>()("onicode/provider/ProviderInstanceRegistryMutator") {}
