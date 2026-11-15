import type { ScopedThreadRef } from "@onicode/contracts";
import type { ReactNode } from "react";

import { environmentThreadDetails } from "../../state/threads";
import {
  NO_WORKSPACE_MUTATION_ID_ATOM,
  WorkspaceMutationIdContext,
} from "./workspaceMutationIdContext";

export function WorkspaceMutationIdProvider(props: {
  readonly threadRef: ScopedThreadRef | null;
  readonly children: ReactNode;
}) {
  return (
    <WorkspaceMutationIdContext
      value={
        props.threadRef === null
          ? NO_WORKSPACE_MUTATION_ID_ATOM
          : environmentThreadDetails.workspaceMutationIdAtom(props.threadRef)
      }
    >
      {props.children}
    </WorkspaceMutationIdContext>
  );
}
