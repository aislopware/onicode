import { act, useLayoutEffect } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";

const navigation = vi.hoisted(() => {
  const route = {
    key: "Thread-1",
    name: "Thread",
    params: { environmentId: "env-1", threadId: "thread-1" },
  };
  // A thread preview's own navigator keeps answering with its last state after it unmounts.
  return { route, state: { index: 0, routes: [route] } };
});

vi.mock("@react-navigation/native", () => ({
  useNavigation: () => ({ getState: () => navigation.state }),
  useRoute: () => navigation.route,
}));

import { useWorkspacePageStillShows } from "./useWorkspacePageStillShows";

it("stops counting a page in the commit that unmounts it", () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  using _globals = { [Symbol.dispose]: () => vi.unstubAllGlobals() };
  const page: { stillShows?: () => boolean } = {};
  const seenByNextPage: boolean[] = [];
  function Page() {
    page.stillShows = useWorkspacePageStillShows();
    return null;
  }
  // A late result that lands while the next page commits, before passive cleanups run.
  function NextPage() {
    useLayoutEffect(() => {
      seenByNextPage.push(page.stillShows!());
    });
    return null;
  }
  const root = createRoot(document.createElement("div"));
  using _root = { [Symbol.dispose]: () => act(() => root.unmount()) };

  act(() => root.render(<Page />));
  expect(page.stillShows!()).toBe(true);
  act(() => root.render(<NextPage />));

  expect(seenByNextPage).toEqual([false]);
  expect(page.stillShows!()).toBe(false);
});
