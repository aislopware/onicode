import { EnvironmentId } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { canOpenRemoteDesktop, resolveRemoteDesktopPageState } from "./remoteDesktopPage.logic";

const primary = EnvironmentId.make("primary");
const remote = EnvironmentId.make("remote");
const host = {
  displays: [{ id: "1", label: "Built-in", width: 2560, height: 1600, primary: true }],
  control: true,
};
const environment = (
  phase: string,
  serverConfig: { readonly remoteDesktop?: boolean } | null,
  environmentId = remote,
) => ({ environmentId, connection: { phase }, serverConfig });

describe("canOpenRemoteDesktop", () => {
  it.each([
    {
      label: "connected server that serves it",
      env: environment("connected", { remoteDesktop: true }),
      dev: false,
      expected: true,
    },
    {
      label: "server too old to serve it",
      env: environment("connected", {}),
      dev: false,
      expected: false,
    },
    {
      label: "not connected",
      env: environment("reconnecting", { remoteDesktop: true }),
      dev: false,
      expected: false,
    },
    {
      label: "this app's own machine",
      env: environment("connected", { remoteDesktop: true }, primary),
      dev: false,
      expected: false,
    },
    {
      label: "this app's own machine in a dev build",
      env: environment("connected", { remoteDesktop: true }, primary),
      dev: true,
      expected: true,
    },
  ])("$label", ({ env, dev, expected }) => {
    expect(canOpenRemoteDesktop(env, primary, dev)).toBe(expected);
  });
});

describe("resolveRemoteDesktopPageState", () => {
  const supported = environment("connected", { remoteDesktop: true });
  it.each([
    {
      label: "an unknown environment",
      input: { environment: null, canConnect: true, state: null, error: null },
      expected: { kind: "unknown-environment" },
    },
    {
      label: "a disconnected environment",
      input: {
        environment: environment("offline", null),
        canConnect: null,
        state: null,
        error: null,
      },
      expected: { kind: "disconnected" },
    },
    {
      label: "a server config still loading",
      input: {
        environment: environment("connected", null),
        canConnect: true,
        state: null,
        error: null,
      },
      expected: { kind: "loading" },
    },
    {
      label: "an old server, before checking permission",
      input: {
        environment: environment("connected", {}),
        canConnect: null,
        state: null,
        error: null,
      },
      expected: { kind: "unsupported" },
    },
    {
      label: "an unknown session",
      input: { environment: supported, canConnect: null, state: null, error: null },
      expected: { kind: "loading" },
    },
    {
      label: "a client without terminal access",
      input: { environment: supported, canConnect: false, state: { host }, error: null },
      expected: { kind: "forbidden" },
    },
    {
      label: "a failed subscription",
      input: { environment: supported, canConnect: true, state: null, error: "boom" },
      expected: { kind: "error", message: "boom" },
    },
    {
      label: "a stale error with live state",
      input: { environment: supported, canConnect: true, state: { host }, error: "boom" },
      expected: { kind: "host", host },
    },
    {
      label: "no host sharing",
      input: { environment: supported, canConnect: true, state: { host: null }, error: null },
      expected: { kind: "no-host" },
    },
  ])("resolves $label", ({ input, expected }) => {
    expect(resolveRemoteDesktopPageState(input)).toEqual(expected);
  });
});
