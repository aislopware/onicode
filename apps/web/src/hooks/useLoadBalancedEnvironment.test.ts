import { EnvironmentId } from "@onicode/contracts";
import { AtomRegistry } from "effect/reactivity";
import { expect, it, vi } from "vite-plus/test";

const host = vi.hoisted(() => ({
  cpu: {} as Record<string, number[]>,
  reads: 0,
}));
vi.mock(import("../state/server"), async () => {
  const { AsyncResult, Atom } = await import("effect/reactivity");
  const hostResources = Atom.family((environmentId: string) =>
    Atom.keepAlive(
      Atom.make(() => {
        host.reads += 1;
        const answers = host.cpu[environmentId]!;
        const cpuUtilization = answers.length > 1 ? answers.shift()! : answers[0]!;
        return AsyncResult.success({
          sampledAt: 0,
          cpuUtilization,
          cpuCount: 4,
          availableMemoryBytes: 8,
          totalMemoryBytes: 16,
        });
      }),
    ),
  );
  return {
    serverEnvironment: {
      hostResources: ({ environmentId }: { environmentId: string }) => hostResources(environmentId),
    },
  } as never;
});

import { hostResourcesAtom, selectLoadBalancedEnvironment } from "./useLoadBalancedEnvironment";

it("samples busy machines again until one frees up", () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
  const registry = AtomRegistry.make();
  try {
    const local = EnvironmentId.make("busy-local");
    const remote = EnvironmentId.make("busy-remote");
    host.cpu = { [local]: [0.99], [remote]: [0.99, 0.2] };
    const atom = hostResourcesAtom(`${local}\n${remote}`);
    registry.mount(atom);
    const select = () => selectLoadBalancedEnvironment(registry.get(atom), {});

    expect(select()).toEqual({ pending: true, environmentId: null, failed: false });

    vi.advanceTimersByTime(5_000);
    expect(select()).toEqual({ pending: false, environmentId: remote, failed: false });

    const reads = host.reads;
    vi.advanceTimersByTime(5_000);
    expect(host.reads).toBe(reads);
  } finally {
    registry.dispose();
    vi.useRealTimers();
  }
});
