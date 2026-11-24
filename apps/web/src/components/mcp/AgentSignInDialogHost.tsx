import { agentSignInFailureMessage } from "@onicode/client-runtime/agent-sign-in-failure";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@onicode/client-runtime/state/runtime";
import {
  type AgentSignInRequest,
  type AuthMcpClientAccess,
  type EnvironmentId,
} from "@onicode/contracts";
import { useAtomValue } from "@effect/atom-react";
import * as DateTime from "effect/DateTime";
import { T, useGT, Var } from "gt-react";
import { useState } from "react";

import { useNowSecond } from "~/hooks/useNowSecond";
import { m } from "~/i18n/messages";
import { useEnvironments } from "~/state/environments";
import { useEnvironmentQuery } from "~/state/query";
import { serverEnvironment } from "~/state/server";
import { useAtomCommand } from "~/state/use-atom-command";
import { Button } from "../ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "../ui/dialog";
import {
  AgentAccessPicker,
  closestGrantable,
  DEFAULT_AGENT_ACCESS,
  useGrantableAgentAccess,
} from "./AgentAccessPicker";

/**
 * Asks to approve an outside agent's sign-in on every connected environment this device may
 * manage access on. Any device can answer; the others close when it does.
 */
export function AgentSignInDialogHost() {
  const { environments } = useEnvironments();
  return environments
    .filter(
      (environment) =>
        environment.connection.phase === "connected" &&
        // Older servers have no sign-in stream to subscribe to.
        environment.serverConfig?.environment.capabilities.webhookSchedules === true,
    )
    .map((environment) => (
      <EnvironmentAgentSignIns
        key={environment.environmentId}
        environmentId={environment.environmentId}
        label={environment.label}
      />
    ));
}

function EnvironmentAgentSignIns({
  environmentId,
  label,
}: {
  readonly environmentId: EnvironmentId;
  readonly label: string;
}) {
  const canAnswer = useAtomValue(serverEnvironment.answerAgentSignIn.permissionAtom(environmentId));
  return canAnswer ? <PendingSignIns environmentId={environmentId} label={label} /> : null;
}

function PendingSignIns({
  environmentId,
  label,
}: {
  readonly environmentId: EnvironmentId;
  readonly label: string;
}) {
  const requests =
    useEnvironmentQuery(serverEnvironment.agentSignIns({ environmentId, input: {} })).data ?? [];
  // Closing without an answer leaves the sign-in to another device or its expiry.
  const [setAside, setSetAside] = useState<ReadonlySet<string>>(() => new Set());
  const waiting = requests.filter((request) => !setAside.has(request.id));
  const current = waiting[0];
  if (current === undefined) return null;
  return (
    <AgentSignInDialog
      key={current.id}
      environmentId={environmentId}
      environmentLabel={label}
      request={current}
      queued={waiting.length - 1}
      onSetAside={() => setSetAside((ids) => new Set(ids).add(current.id))}
    />
  );
}

function AgentSignInDialog({
  environmentId,
  environmentLabel,
  request,
  queued,
  onSetAside,
}: {
  readonly environmentId: EnvironmentId;
  readonly environmentLabel: string;
  readonly request: AgentSignInRequest;
  readonly queued: number;
  readonly onSetAside: () => void;
}) {
  const gt = useGT();
  const denied = useGrantableAgentAccess(environmentId, environmentLabel);
  const [picked, setPicked] = useState<AuthMcpClientAccess | null>(null);
  const access = picked ?? closestGrantable(DEFAULT_AGENT_ACCESS, denied);
  const [pending, setPending] = useState<"approve" | "deny" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const answer = useAtomCommand(serverEnvironment.answerAgentSignIn, { reportFailure: false });
  const now = useNowSecond();
  const left = Math.max(0, Math.floor((DateTime.toEpochMillis(request.expiresAt) - now) / 1000));

  const respond = async (decision: "approve" | "deny") => {
    if (decision === "approve" && access === null) return;
    setPending(decision);
    setError(null);
    const result = await answer({
      environmentId,
      input: {
        id: request.id,
        answer:
          decision === "approve" && access !== null
            ? { _tag: "approve", access }
            : { _tag: "deny" },
      },
    });
    setPending(null);
    // On success the request leaves the stream and this dialog unmounts.
    if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
      setError(agentSignInFailureMessage(squashAtomCommandFailure(result), m));
    }
  };

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && pending === null) onSetAside();
      }}
    >
      <DialogPopup className="max-w-md">
        <DialogHeader>
          <DialogTitle>
            <T>
              Allow <Var>{request.clientName}</Var> to use Onicode?
            </T>
          </DialogTitle>
          <DialogDescription>
            <T>
              An app is signing in to <Var>{environmentLabel}</Var>. Approve only if its sign-in
              page shows this code.
            </T>
          </DialogDescription>
        </DialogHeader>
        <DialogPanel scrollFade={false}>
          <div className="space-y-4">
            <p
              className="text-center font-mono text-2xl font-semibold tracking-widest"
              aria-label={gt("Match code {code}", { code: request.matchCode })}
            >
              {request.matchCode}
            </p>
            {request.loopbackRedirect ? (
              <p className="text-sm text-muted-foreground">
                <T>It returns to an app on the computer where the sign-in page opened.</T>
              </p>
            ) : (
              <p className="text-sm text-warning-foreground">
                <T>
                  It returns to <Var>{request.redirectHost}</Var>, which gets this access. Approve
                  only if you started this sign-in there.
                </T>
              </p>
            )}
            {access === null ? (
              <p className="text-sm text-destructive-foreground">
                {denied.get(DEFAULT_AGENT_ACCESS) ?? gt("This device cannot grant any access.")}
              </p>
            ) : (
              <AgentAccessPicker
                label={gt("What it may do")}
                value={access}
                denied={denied}
                onChange={setPicked}
                disabled={pending !== null}
              />
            )}
            <p className="text-xs text-muted-foreground">
              {queued > 0
                ? gt("Expires in {time} · {count} more waiting", {
                    time: `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`,
                    count: queued,
                  })
                : gt("Expires in {time}", {
                    time: `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`,
                  })}
            </p>
            {error ? (
              <p className="text-sm text-destructive-foreground" role="alert">
                {error}
              </p>
            ) : null}
          </div>
        </DialogPanel>
        <DialogFooter>
          <Button
            variant="outline"
            disabled={pending !== null}
            onClick={() => void respond("deny")}
          >
            {pending === "deny" ? gt("Denying…") : gt("Deny")}
          </Button>
          <Button
            disabled={pending !== null || access === null}
            onClick={() => void respond("approve")}
          >
            {pending === "approve" ? gt("Approving…") : gt("Approve")}
          </Button>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
}
