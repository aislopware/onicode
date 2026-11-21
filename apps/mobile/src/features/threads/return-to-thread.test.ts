import * as NodeModule from "node:module";
import { expect, it, vi } from "vite-plus/test";

vi.mock("@react-navigation/native", async () => {
  const { createRequire } = await import("node:module");
  const requireNavigation = createRequire(
    createRequire(import.meta.url).resolve("@react-navigation/native/package.json"),
  );
  return { StackActions: requireNavigation("@react-navigation/routers").StackActions };
});

import { returnToThreadAction } from "./return-to-thread";

const requireNavigation = NodeModule.createRequire(
  NodeModule.createRequire(import.meta.url).resolve("@react-navigation/native/package.json"),
);
const { StackRouter, StackActions } = requireNavigation("@react-navigation/routers") as {
  StackRouter: typeof import("@react-navigation/native").StackRouter;
  StackActions: typeof import("@react-navigation/native").StackActions;
};

const thread = { environmentId: "environment", threadId: "thread" };

function stack(...pushes: ReadonlyArray<readonly [string, object]>) {
  const router = StackRouter({});
  const options = {
    routeNames: ["Home", "Thread", "ThreadFile"],
    routeParamList: {},
    routeGetIdList: {},
  };
  let state = router.getInitialState(options);
  const apply = (action: Parameters<typeof router.getStateForAction>[1]) => {
    const next = router.getStateForAction(state, action, options);
    if (!next) throw new Error(`Rejected ${action.type}`);
    state = router.getRehydratedState(next, options);
  };
  for (const [name, params] of pushes) apply(StackActions.push(name, params));
  return {
    apply,
    get routes() {
      return state.routes;
    },
  };
}

it("returns to the thread a file was opened from instead of stacking the thread on itself", () => {
  const inspector = { type: "device", ...thread };
  const screen = stack(
    ["Thread", { ...thread, inspector }],
    ["ThreadFile", { ...thread, path: ["src"] }],
    ["ThreadFile", { ...thread, path: ["src", "math.ts"] }],
  );
  const threadKey = screen.routes[1]!.key;
  screen.apply(returnToThreadAction(screen.routes, thread));
  expect(screen.routes.map((route) => route.name)).toEqual(["Home", "Thread"]);
  expect(screen.routes[1]).toMatchObject({ key: threadKey, params: { ...thread, inspector } });
});

it.each([
  ["no thread", []],
  ["another thread", [["Thread", { ...thread, threadId: "parent" }]]],
] as const)("puts the thread in the file's place over %s", (_name, below) => {
  const screen = stack(...below, ["ThreadFile", { ...thread, path: ["math.ts"] }]);
  const kept = screen.routes.slice(0, -1);
  screen.apply(returnToThreadAction(screen.routes, thread));
  expect(screen.routes.slice(0, -1)).toEqual(kept);
  expect(screen.routes.at(-1)).toMatchObject({ name: "Thread", params: thread });
});
