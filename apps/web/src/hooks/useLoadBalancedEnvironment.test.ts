import { EnvironmentId } from "@onicode/contracts";
import { AtomRegistry } from "effect/reactivity";
import { expect, it, vi } from "vite-plus/test";

const host = vi.hoisted(() => ({
  cpu: {} as Record<string, number[] | "fails">,
  reads: {} as Record<string, number>,
}));
vi.mock(import("../state/server"), async () => {
  const { AsyncResult, Atom } = await import("effect/reactivity");
  const { Cause } = await import("effect");
  const hostResources = Atom.family((environmentId: string) =>
    Atom.keepAlive(
      Atom.make(() => {
        host.reads[environmentId] = (host.reads[environmentId] ?? 0) + 1;
        const answers = host.cpu[environmentId]!;
        if (answers === "fails") return AsyncResult.failure(Cause.fail("no host resources"));
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

import { loadBalancedSelectionAtom } from "./useLoadBalancedEnvironment";

it.each([
  { name: "busy machines", cpu: { a: [0.99], b: [0.99, 0.2] }, picks: "b", failed: false },
  { name: "a failing machine", cpu: { a: "fails", b: [0.99, 0.2] }, picks: "b", failed: false },
  { name: "only failing machines", cpu: { a: "fails", b: "fails" }, picks: null, failed: true },
] as const)("samples $name again until one frees up", ({ cpu, picks, failed }) => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
  const registry = AtomRegistry.make();
  try {
    host.cpu = Object.fromEntries(
      Object.entries(cpu).map(([id, answers]) => [
        id,
        answers === "fails" ? answers : [...answers],
      ]),
    );
    host.reads = {};
    const atom = loadBalancedSelectionAtom("a\nb\t50,50");
    registry.mount(atom);
    if (!failed) {
      expect(registry.get(atom)).toEqual({ pending: true, environmentId: null, failed: false });
      vi.advanceTimersByTime(5_000);
    }
    expect(registry.get(atom)).toEqual({
      pending: false,
      environmentId: picks === null ? null : EnvironmentId.make(picks),
      failed,
    });

    const reads = { ...host.reads };
    vi.advanceTimersByTime(5_000);
    expect(host.reads).toEqual(reads);
    // A machine that fails is not asked again on every sample.
    for (const [id, answers] of Object.entries(cpu)) {
      if (answers === "fails") expect(reads[id]).toBe(1);
    }
  } finally {
    registry.dispose();
    vi.useRealTimers();
  }
});
