import { useAtomValue } from "@effect/atom-react";
import { managedCapabilityDenial } from "@onicode/client-runtime/contract-labels";
import { squashAtomCommandFailure } from "@onicode/client-runtime/state/runtime";
import {
  AuthFilesystemReadScope,
  type EnvironmentId,
  type ProjectId,
  type WorktreeLayoutCommand,
  type WorktreeLayoutCommandKind,
} from "@onicode/contracts";
import { AsyncResult, Atom } from "effect/reactivity";
import { msg, T, useGT, useMessages } from "gt-react";
import { useState } from "react";

import type { SidebarProjectGroupMember } from "../../sidebarProjectGrouping";
import { useEnvironments } from "../../state/environments";
import { filesystemEnvironment } from "../../state/filesystem";
import { useManagedPolicy } from "../../state/managedPolicy";
import { useEnvironmentQuery } from "../../state/query";
import { useEnvironmentsWithScope } from "../../state/session";
import { useAtomCommand } from "../../state/use-atom-command";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
import { toastManager } from "../ui/toast";
import { WORKTREE_LAYOUT_COMMAND_LABELS } from "../WorktreeLayoutApprovalDialogHost";
import { SettingsRow, SettingsSection } from "./settingsLayout";

const COMMAND_TITLES: Record<WorktreeLayoutCommandKind, string> = {
  create: msg("Create command"),
  remove: msg("Remove command"),
};

const checkoutLayoutCommandsAtom = Atom.family((key: string) => {
  const targets = JSON.parse(key) as ReadonlyArray<{
    readonly environmentId: EnvironmentId;
    readonly projectId: ProjectId;
  }>;
  return Atom.make((get) =>
    targets.map(({ environmentId, projectId }) =>
      get(filesystemEnvironment.layoutCommands({ environmentId, input: { projectId } })),
    ),
  ).pipe(Atom.withLabel(`project-worktree-layout-commands:${key}`));
});

/** The `worktreeLayout` commands each checkout's `onicode.json` declares, and whether they may run. */
export function ProjectWorktreeLayoutSettings({
  members,
}: {
  members: ReadonlyArray<SidebarProjectGroupMember>;
}) {
  const gt = useGT();
  const checkouts = members.filter((member) => !member.isScratch);
  const readableEnvironmentIds = useEnvironmentsWithScope(checkouts, AuthFilesystemReadScope);
  const readableCheckouts = checkouts.filter((member) =>
    readableEnvironmentIds.has(member.environmentId),
  );
  const results = useAtomValue(
    checkoutLayoutCommandsAtom(
      JSON.stringify(
        readableCheckouts.map((member) => ({
          environmentId: member.environmentId,
          projectId: member.id,
        })),
      ),
    ),
  );
  const visibleCheckouts = readableCheckouts.filter((_, index) => {
    const result = results[index];
    if (result === undefined) return false;
    const data = AsyncResult.getOrElse(result, () => null);
    return (data === null && result._tag === "Failure") || (data?.commands.length ?? 0) > 0;
  });
  // Most projects declare none, and then there is nothing to show.
  if (visibleCheckouts.length === 0) return null;
  return (
    <SettingsSection id="project-worktree-layout" title={gt("Worktree layout")}>
      {visibleCheckouts.map((member) => (
        <CheckoutWorktreeLayout
          key={member.physicalProjectKey}
          member={member}
          showCheckout={checkouts.length > 1}
        />
      ))}
    </SettingsSection>
  );
}

function CheckoutWorktreeLayout({
  member,
  showCheckout,
}: {
  member: SidebarProjectGroupMember;
  showCheckout: boolean;
}) {
  const gt = useGT();
  const m = useMessages();
  const environmentId = member.environmentId;
  const { environments } = useEnvironments();
  const isConnected =
    environments.find((entry) => entry.environmentId === environmentId)?.connection.phase ===
    "connected";
  const canApprove = useAtomValue(
    filesystemEnvironment.approveLayoutCommands.permissionAtom(environmentId),
  );
  const policyDenial = managedCapabilityDenial(
    useManagedPolicy(environmentId),
    "worktreeLayoutCommands",
    m,
  );
  const query = useEnvironmentQuery(
    filesystemEnvironment.layoutCommands({ environmentId, input: { projectId: member.id } }),
    m,
  );
  const approve = useAtomCommand(filesystemEnvironment.approveLayoutCommands, {
    reportFailure: false,
  });
  const [savingKind, setSavingKind] = useState<WorktreeLayoutCommandKind | null>(null);
  const commands = query.data?.commands ?? [];
  // A failed first read is not a project without commands.
  if (query.data === null && query.error !== null) {
    return (
      <SettingsRow
        title={gt("Could not load the worktree layout commands")}
        description={query.error}
        status={
          showCheckout
            ? `${member.environmentLabel ?? gt("Environment")} · ${member.workspaceRoot}`
            : undefined
        }
        control={
          <Button size="xs" variant="outline" onClick={query.refresh}>
            <T>Retry</T>
          </Button>
        }
      />
    );
  }
  if (commands.length === 0) return null;

  // Approving sends the text on screen, so a file that changed since is not approved unseen.
  const save = async (command: WorktreeLayoutCommand, approved: boolean) => {
    setSavingKind(command.kind);
    const result = await approve({
      environmentId,
      input: {
        projectId: member.id,
        approvals: [{ kind: command.kind, command: approved ? command.command : null }],
      },
    });
    setSavingKind(null);
    if (result._tag === "Failure") {
      const error = squashAtomCommandFailure(result);
      toastManager.add({
        type: "error",
        title: approved ? gt("Could not approve the command") : gt("Could not revoke the approval"),
        ...(error instanceof Error ? { description: error.message } : {}),
      });
    }
    query.refresh();
  };

  return (
    <>
      <p className="px-3 py-2.5 text-xs text-muted-foreground sm:px-4">
        {policyDenial ?? (
          <T>
            This project's <code>onicode.json</code> sets up worktree threads with its own commands.
            They run on this environment's machine with your permissions, and only as approved here:
            a command whose text changes needs approval again.
          </T>
        )}
        {showCheckout ? (
          <span className="mt-1 block truncate">
            {member.environmentLabel ?? gt("Environment")} · {member.workspaceRoot}
          </span>
        ) : null}
      </p>
      {commands.map((command) => (
        <SettingsRow
          key={command.kind}
          title={
            <span className="flex min-w-0 items-center gap-2">
              <span className="min-w-0 truncate">{m(COMMAND_TITLES[command.kind])}</span>
              {policyDenial !== undefined ? (
                <Badge size="sm" variant="outline">
                  <T>Off</T>
                </Badge>
              ) : command.approved ? (
                <Badge size="sm" variant="success">
                  <T>Approved</T>
                </Badge>
              ) : (
                <Badge size="sm" variant="warning">
                  <T>Needs approval</T>
                </Badge>
              )}
            </span>
          }
          description={m(WORKTREE_LAYOUT_COMMAND_LABELS[command.kind])}
          control={
            <Button
              size="xs"
              variant={command.approved ? "ghost" : "outline"}
              disabled={
                !canApprove ||
                !isConnected ||
                savingKind !== null ||
                query.isPending ||
                (!command.approved && policyDenial !== undefined)
              }
              aria-label={
                command.approved
                  ? gt("Revoke approval of the {kind} command", { kind: command.kind })
                  : gt("Approve the {kind} command", { kind: command.kind })
              }
              onClick={() => void save(command, !command.approved)}
            >
              {command.approved ? <T>Revoke</T> : <T>Approve</T>}
            </Button>
          }
        >
          <pre className="mb-2 max-h-48 overflow-auto whitespace-pre-wrap wrap-anywhere rounded-md border bg-muted/40 px-3 py-2 font-mono text-xs">
            {command.command}
          </pre>
        </SettingsRow>
      ))}
      {!canApprove ? (
        <p className="px-3 py-2 text-xs text-muted-foreground sm:px-4">
          <T>This connection cannot approve commands. Use a client that can use the terminal.</T>
        </p>
      ) : null}
      {query.error ? (
        <p role="alert" className="px-3 py-2 text-xs text-warning-foreground sm:px-4">
          {query.error}
        </p>
      ) : null}
    </>
  );
}
