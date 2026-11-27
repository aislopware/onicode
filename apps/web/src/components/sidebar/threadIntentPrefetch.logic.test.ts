import { EnvironmentId } from "@onicode/contracts";
import { expect, it } from "vite-plus/test";

import { prefetchesOnHover } from "./threadIntentPrefetch.logic";

const local = EnvironmentId.make("local");
const remote = EnvironmentId.make("remote");

it.each([
  { pointerType: "mouse", environmentId: local, localEnvironmentId: local, expected: true },
  { pointerType: "mouse", environmentId: remote, localEnvironmentId: local, expected: false },
  { pointerType: "mouse", environmentId: remote, localEnvironmentId: null, expected: false },
  { pointerType: "touch", environmentId: local, localEnvironmentId: local, expected: false },
  { pointerType: "pen", environmentId: local, localEnvironmentId: local, expected: false },
])(
  "hover prefetches a $pointerType row of $environmentId with local $localEnvironmentId: $expected",
  ({ expected, ...input }) => {
    expect(prefetchesOnHover(input)).toBe(expected);
  },
);
