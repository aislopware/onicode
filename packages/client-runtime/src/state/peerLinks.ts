import {
  type EnvironmentId,
  type PeerLinkSummary,
  type PeerLinkThreadInput,
  type ServerConfig,
  WS_METHODS,
} from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Stream from "effect/Stream";
import { AsyncResult, Atom, type AtomRegistry } from "effect/reactivity";

import type { EnvironmentRegistry } from "../connection/registry.ts";
import { offersLinkedEnvironments } from "../linkRequest.ts";
import {
  followLinkedThread,
  endLinkedThreadView,
  keepLinkedThreadView,
  type LinkedThreadView,
} from "../linkedThread.ts";
import { request, subscribe } from "../rpc/client.ts";
import {
  createEnvironmentRpcCommand,
  createEnvironmentRpcQueryAtomFamily,
  followStreamInEnvironment,
} from "./runtime.ts";

/** The environment a client reads through, and the linked environment's thread it reads. */
export interface LinkedThreadViewTarget {
  readonly environmentId: EnvironmentId;
  readonly input: PeerLinkThreadInput;
}

const threadViewKey = ({ environmentId, input }: LinkedThreadViewTarget) =>
  JSON.stringify({
    environmentId,
    input: { environmentId: input.environmentId, threadId: input.threadId },
  });

export function createPeerLinkEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>,
) {
  const list = createEnvironmentRpcQueryAtomFamily(runtime, {
    label: "environment-data:peer-links:list",
    tag: WS_METHODS.peerLinksList,
    // Listing probes every linked environment, so a list stays fresh briefly.
    staleTimeMs: 10_000,
  });
  // Names only, for opening linked threads with thread read access; it probes nothing.
  const names = createEnvironmentRpcQueryAtomFamily(runtime, {
    label: "environment-data:peer-links:names",
    tag: WS_METHODS.peerLinksNames,
    staleTimeMs: 10_000,
  });
  const refreshLinks = (registry: AtomRegistry.AtomRegistry, environmentId: EnvironmentId) =>
    Effect.sync(() => {
      registry.refresh(list({ environmentId, input: {} }));
      registry.refresh(names({ environmentId, input: {} }));
    });
  /**
   * What a view of the thread last showed. It outlives the subscription for a while, so coming
   * back to the view shows the thread at once while the stream starts again.
   */
  const lastThreadView = Atom.family((key: string) =>
    Atom.make<LinkedThreadView | null>(null).pipe(
      Atom.setIdleTTL(5 * 60_000),
      Atom.withLabel(`environment-data:peer-links:last-thread-view:${key}`),
    ),
  );
  /**
   * A linked environment's thread, streamed while a view reads it. The subscription ends with its
   * last reader, which closes the stream at the linked environment too.
   */
  const threadView = Atom.family((key: string) => {
    const { environmentId, input } = JSON.parse(key) as LinkedThreadViewTarget;
    const kept = lastThreadView(key);
    const stream = runtime
      .atom(({ registry }) =>
        followStreamInEnvironment(
          environmentId,
          Stream.suspend(() =>
            followLinkedThread({
              items: subscribe(WS_METHODS.peerLinksSubscribeThread, input),
              historyPage: (cursor) =>
                request(WS_METHODS.peerLinksThreadHistoryPage, { ...input, cursor }),
              previous: registry.get(kept),
            }),
          ),
        ),
      )
      .pipe(Atom.setIdleTTL(0), Atom.withLabel(`environment-data:peer-links:thread-stream:${key}`));
    return Atom.readable(
      (get) => {
        const live = get(stream);
        if (AsyncResult.isSuccess(live)) {
          get.set(kept, keepLinkedThreadView(get.once(kept), live.value));
        } else if (AsyncResult.isFailure(live)) {
          const last = get.once(kept);
          const ended = last === null ? null : endLinkedThreadView(last);
          if (ended !== last) get.set(kept, ended);
        }
        return live;
      },
      (refresh) => refresh(stream),
    ).pipe(Atom.setIdleTTL(0), Atom.withLabel(`environment-data:peer-links:thread-view:${key}`));
  });
  return {
    list,
    names,
    link: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:peer-links:link",
      tag: WS_METHODS.peerLinksLink,
      onSuccess: ({ environmentId }, registry) => refreshLinks(registry, environmentId),
    }),
    // Leaves the list alone: a batch refreshes it once, when it ends (`runLinkBatch`).
    linkInBatch: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:peer-links:link-in-batch",
      tag: WS_METHODS.peerLinksLink,
    }),
    unlink: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:peer-links:unlink",
      tag: WS_METHODS.peerLinksUnlink,
      onSuccess: ({ environmentId }, registry) => refreshLinks(registry, environmentId),
    }),
    threadView: (target: LinkedThreadViewTarget) => threadView(threadViewKey(target)),
    lastThreadView: (target: LinkedThreadViewTarget) => lastThreadView(threadViewKey(target)),
    /** One item of a linked environment's thread in full, for a row the reader opened. */
    threadItem: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:peer-links:thread-item",
      tag: WS_METHODS.peerLinksThreadItem,
      idleTtlMs: 0,
    }),
    /** Asks for a one-time pairing with a linked environment. Its answer is a credential: never cached. */
    pairClient: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:peer-links:pair-client",
      tag: WS_METHODS.peerLinksPairClient,
      concurrency: {
        mode: "singleFlight",
        key: ({ environmentId, input }) => JSON.stringify([environmentId, input.environmentId]),
      },
    }),
    /** Asks for a pairing code from a linked environment, to link another one there. Never cached. */
    pairLink: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:peer-links:pair-link",
      tag: WS_METHODS.peerLinksPairLink,
    }),
    // An agent's link card. One answer per card at a time.
    answerRequest: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:peer-links:answer-request",
      tag: WS_METHODS.peerLinksAnswerRequest,
      concurrency: {
        mode: "singleFlight",
        key: ({ environmentId, input }) =>
          JSON.stringify([environmentId, input.threadId, input.turnItemId]),
      },
      onSuccess: ({ environmentId }, registry) => refreshLinks(registry, environmentId),
    }),
  };
}

const EXPIRY_WARNING_DAYS = 5;

export type PeerLinkHealth =
  | { readonly kind: "reachable"; readonly expiresInDays: number | null }
  | { readonly kind: "unreachable"; readonly detail: string | null }
  | { readonly kind: "failing"; readonly detail: string }
  | { readonly kind: "expired" };

/**
 * Listing only checks that the peer answers, so a link whose last call failed
 * (the peer revoked it, say) is `failing` with the recorded reason; the next
 * successful call clears it.
 */
export function peerLinkHealth(link: PeerLinkSummary, now: DateTime.Utc): PeerLinkHealth {
  if (link.status === "expired" || DateTime.isLessThanOrEqualTo(link.expiresAt, now)) {
    return { kind: "expired" };
  }
  if (link.status === "unreachable") return { kind: "unreachable", detail: link.lastError };
  if (link.lastError !== null) return { kind: "failing", detail: link.lastError };
  const days = Math.ceil(Duration.toDays(DateTime.distance(now, link.expiresAt)));
  return { kind: "reachable", expiresInDays: days <= EXPIRY_WARNING_DAYS ? days : null };
}

/** A linked machine, and the environment that can get a pairing code from it. */
export interface LinkThroughRoute {
  readonly viaEnvironmentId: EnvironmentId;
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly urls: ReadonlyArray<string>;
}

/**
 * Whether an environment offers links and can get a pairing code from a machine it links to,
 * for another environment's link there.
 */
export const offersLinkThrough = (
  config: Pick<ServerConfig, "environment" | "settings"> | null | undefined,
): boolean =>
  offersLinkedEnvironments(config) && config?.environment.capabilities.linkPairingForLink === true;

/**
 * The machines `sourceEnvironmentId` can link to without a pasted code: those another of the
 * client's environments reaches over a full-access link that answers. `hosts` are environments
 * the client is connected to and manages access on, the only ones it may ask.
 */
export function linkThroughRoutes(input: {
  readonly sourceEnvironmentId: EnvironmentId;
  readonly hosts: ReadonlyArray<{
    readonly environmentId: EnvironmentId;
    readonly config: Pick<ServerConfig, "environment" | "settings"> | null;
    readonly links: ReadonlyArray<PeerLinkSummary>;
  }>;
  readonly now: DateTime.Utc;
}): ReadonlyMap<EnvironmentId, LinkThroughRoute> {
  const routes = new Map<EnvironmentId, LinkThroughRoute>();
  for (const host of input.hosts) {
    if (host.environmentId === input.sourceEnvironmentId || !offersLinkThrough(host.config)) {
      continue;
    }
    for (const link of host.links) {
      if (
        link.environmentId === input.sourceEnvironmentId ||
        link.access !== "full-access" ||
        routes.has(link.environmentId) ||
        peerLinkHealth(link, input.now).kind !== "reachable"
      ) {
        continue;
      }
      routes.set(link.environmentId, {
        viaEnvironmentId: host.environmentId,
        environmentId: link.environmentId,
        label: link.label,
        urls: link.urls,
      });
    }
  }
  return routes;
}
