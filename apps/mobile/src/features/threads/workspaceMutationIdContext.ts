import { Atom } from "effect/reactivity";
import { createContext } from "react";

export const NO_WORKSPACE_MUTATION_ID_ATOM = Atom.make<string | null | undefined>(undefined);

/**
 * The atom of the thread's workspace mutation id, so a host image the agent rewrites gets a fresh
 * URL. The id changes after every tool call: as a context value it made React search the whole
 * feed for readers each time, so the context carries the atom and the image reads it.
 */
export const WorkspaceMutationIdContext = createContext<Atom.Atom<string | null | undefined>>(
  NO_WORKSPACE_MUTATION_ID_ATOM,
);
