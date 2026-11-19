import { EnvironmentId, ThreadId } from "@onicode/contracts";
import type { AsyncResult, Atom } from "effect/reactivity";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";

type Answer = { readonly threads: ReadonlyArray<{ readonly id: ThreadId }> };

const fixture = vi.hoisted(() => ({
  focused: true,
  read: null as unknown,
  query: { data: null as Answer | null, error: null as string | null },
}));

vi.mock("@react-navigation/native", () => ({
  useRoute: () => ({ key: "issue" }),
  useNavigationState: <T,>(select: (state: unknown) => T) =>
    select({ index: 0, routes: [{ key: fixture.focused ? "issue" : "thread" }] }),
}));
vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: unknown) => (atom === "hidden" ? new Set() : atom === "refs" ? [] : false),
}));
vi.mock("./threads", () => ({
  environmentThreadShells: {
    hiddenThreadKeysAtom: "hidden",
    anyEnvironmentHidingAtom: "hiding",
    environmentThreadRefsAtom: () => "refs",
  },
}));
vi.mock("./query", () => ({
  useEnvironmentQuery: (atom: unknown) => {
    fixture.read = atom;
    return atom === null
      ? { data: null, error: null, isPending: false, refresh: () => {} }
      : { ...fixture.query, isPending: false, refresh: () => {} };
  },
}));

import { useLinkedThreads } from "./use-linked-threads";

const ENVIRONMENT = EnvironmentId.make("environment");
// The mocked query reads atoms by identity only.
const ATOM = "atom" as unknown as Atom.Atom<AsyncResult.AsyncResult<Answer, never>>;
const ANSWER: Answer = { threads: [{ id: ThreadId.make("thread-1") }] };

function mount() {
  let view: ReturnType<typeof useLinkedThreads> | null = null;
  function Probe() {
    view = useLinkedThreads(ENVIRONMENT, ATOM, "issue ENG-7");
    return null;
  }
  const root = createRoot(document.createElement("div"));
  act(() => root.render(<Probe />));
  return {
    get view() {
      return view!;
    },
    rerender: () => act(() => root.render(<Probe />)),
    [Symbol.dispose]: () => act(() => root.unmount()),
  };
}

it("keeps the last answer through a failed reread, and reports a failed first read", () => {
  fixture.focused = true;
  fixture.query = { data: null, error: "The connection dropped before this finished." };
  using screen = mount();
  expect(screen.view).toMatchObject({ known: false, threads: [] });
  expect(screen.view.error).toBe("The connection dropped before this finished.");

  fixture.query = { data: ANSWER, error: null };
  screen.rerender();
  expect(screen.view).toMatchObject({ known: true, error: null, threads: ANSWER.threads });

  fixture.query = { data: null, error: "The connection dropped before this finished." };
  screen.rerender();
  expect(screen.view).toMatchObject({ known: true, error: null, threads: ANSWER.threads });
});

it("stops reading while another screen covers it, and keeps what it showed", () => {
  fixture.focused = true;
  fixture.query = { data: ANSWER, error: null };
  using screen = mount();
  expect(fixture.read).toBe("atom");

  fixture.focused = false;
  screen.rerender();
  expect(fixture.read).toBeNull();
  expect(screen.view).toMatchObject({ known: true, threads: ANSWER.threads });
});
