import {
  managedCapabilityDenial,
  managedRuntimeModeDenial,
} from "@onicode/client-runtime/contract-labels";
import {
  AuthAccessWriteScope,
  AuthMcpClientAccess,
  type EnvironmentId,
  mcpClientScopes,
  sessionGrantsScope,
} from "@onicode/contracts";
import { useGT, useMessages } from "gt-react";
import { useId } from "react";

import { useManagedPolicy } from "~/state/managedPolicy";
import { useEnvironmentScope, useEnvironmentSessionState } from "~/state/session";
import { accessConfig } from "../settings/linkAccess";
import { Select, SelectItem, SelectPopup, SelectTrigger, SelectValue } from "../ui/select";

/** The level an approval starts at: Supervised, the one that asks before acting. */
export const DEFAULT_AGENT_ACCESS: AuthMcpClientAccess = "approval-required";

/**
 * Why this device cannot grant each level on `environmentId`; absent when it can. The server
 * checks the same: a device never grants more than its own session holds.
 */
export function useGrantableAgentAccess(environmentId: EnvironmentId, environmentLabel: string) {
  const session = useEnvironmentSessionState(environmentId).data;
  const canGrant = useEnvironmentScope(environmentId, AuthAccessWriteScope);
  const policy = useManagedPolicy(environmentId);
  const gt = useGT();
  const m = useMessages();
  const remoteDenial = managedCapabilityDenial(policy, "remoteClients", m);
  return new Map(
    AuthMcpClientAccess.literals.flatMap((level) => {
      const reason =
        remoteDenial ??
        (!canGrant
          ? gt("This device needs the Manage access permission on {environment}.", {
              environment: environmentLabel,
            })
          : session === null ||
              !mcpClientScopes(level).every((scope) => sessionGrantsScope(session, scope))
            ? gt("This device cannot grant this on {environment}.", {
                environment: environmentLabel,
              })
            : level === "read-only"
              ? undefined
              : managedRuntimeModeDenial(policy, level, m));
      return reason === undefined ? [] : [[level, reason] as const];
    }),
  );
}

/** The grantable level closest below `wanted`, or null when none can be granted. */
export function closestGrantable(
  wanted: AuthMcpClientAccess,
  denied: ReadonlyMap<AuthMcpClientAccess, string>,
): AuthMcpClientAccess | null {
  const levels = AuthMcpClientAccess.literals;
  for (let index = levels.indexOf(wanted); index >= 0; index -= 1) {
    const level = levels[index]!;
    if (!denied.has(level)) return level;
  }
  return null;
}

export function AgentAccessPicker({
  label,
  value,
  denied,
  onChange,
  disabled = false,
}: {
  readonly label: string;
  readonly value: AuthMcpClientAccess;
  readonly denied: ReadonlyMap<AuthMcpClientAccess, string>;
  readonly onChange: (access: AuthMcpClientAccess) => void;
  readonly disabled?: boolean;
}) {
  const labelId = useId();
  const m = useMessages();
  const selected = accessConfig[value];
  return (
    <div className="space-y-1.5">
      <span id={labelId} className="block text-xs font-medium">
        {label}
      </span>
      <Select
        value={value}
        disabled={disabled}
        onValueChange={(next) => {
          if (next !== null) onChange(next as AuthMcpClientAccess);
        }}
      >
        <SelectTrigger aria-labelledby={labelId}>
          <SelectValue>
            <span className="flex items-center gap-2">
              <selected.icon aria-hidden className="size-4" />
              {m(selected.label)}
            </span>
          </SelectValue>
        </SelectTrigger>
        <SelectPopup alignItemWithTrigger={false}>
          {AuthMcpClientAccess.literals.map((option) => {
            const { label: optionLabel, description, icon: Icon } = accessConfig[option];
            const reason = denied.get(option);
            return (
              <SelectItem key={option} value={option} disabled={reason !== undefined} hideIndicator>
                <span className="grid min-w-0 gap-0.5 py-0.5">
                  <span className="inline-flex items-center gap-1.5 font-medium text-foreground">
                    <Icon aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
                    {m(optionLabel)}
                  </span>
                  <span className="text-xs leading-4 text-muted-foreground">
                    {reason ?? m(description)}
                  </span>
                </span>
              </SelectItem>
            );
          })}
        </SelectPopup>
      </Select>
      <p className="text-xs text-muted-foreground">{m(selected.description)}</p>
    </div>
  );
}
