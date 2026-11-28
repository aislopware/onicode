import { AuthTerminalOperateScope, type EnvironmentId } from "@onicode/contracts";
import * as Schema from "effect/Schema";
import { msg } from "gt-react";

import { m } from "~/i18n/messages";
import { readLocalApi } from "~/localApi";
import { useEnvironment } from "~/state/environments";
import {
  canForwardPorts,
  forwardedUrl,
  forwardPort,
  useForwardLocality,
} from "~/state/portForwards";
import { useEnvironmentScope } from "~/state/session";

import { namesLoopback, resolveExternalLinkUrl } from "./browserTargetResolver";

/** The port of an http(s) URL on a loopback name; null for any other URL. */
export function loopbackWebPort(url: string): number | null {
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
    if (!namesLoopback(parsed.hostname)) return null;
    return Number(parsed.port || (parsed.protocol === "https:" ? 443 : 80));
  } catch {
    return null;
  }
}

/** Neither a forward nor an address on the environment's network reaches its loopback from here. */
export class LoopbackUnreachableError extends Schema.TaggedError<LoopbackUnreachableError>()(
  "LoopbackUnreachableError",
  { message: Schema.String },
) {}

export const isLoopbackUnreachableError = Schema.is(LoopbackUnreachableError);

/**
 * Opens a URL from an environment's output in the system browser. That browser reaches another
 * machine's loopback only through a desktop forward or the environment's private address.
 */
export function useOpenInSystemBrowser(
  environmentId: EnvironmentId | null,
): (url: string) => Promise<void> {
  const environment = useEnvironment(environmentId);
  const forwardable =
    useEnvironmentScope(environmentId, AuthTerminalOperateScope) &&
    environment?.connection.phase === "connected" &&
    canForwardPorts(environment);
  const locality = useForwardLocality(forwardable ? environmentId : null);
  return async (url) => {
    const api = readLocalApi();
    if (!api) throw new Error(m(msg("Link opening is unavailable.")));
    const port = loopbackWebPort(url);
    if (environmentId === null || port === null) {
      await api.shell.openExternal(url);
      return;
    }
    // Starting a forward runs the probe; only a confirmed local environment opens directly.
    const directUrl =
      forwardable && locality === "local" ? url : resolveExternalLinkUrl(environmentId, url);
    const target =
      forwardable && locality !== "local"
        ? // A server the system browser reaches directly still opens when the forward fails.
          await forwardPort(environmentId, port).then(
            (forward) => forwardedUrl(forward, url),
            (error: unknown) => {
              if (directUrl === null) throw error;
              return directUrl;
            },
          )
        : directUrl;
    if (target === null) {
      throw new LoopbackUnreachableError({
        message: m(
          msg(
            "localhost:{port} runs on another machine, and port forwarding isn't available for it right now.",
            { port },
          ),
        ),
      });
    }
    await api.shell.openExternal(target);
  };
}
