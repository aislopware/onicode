import { agentSignInFailureMessage } from "@onicode/client-runtime/agent-sign-in-failure";
import { useAtomValue } from "@effect/atom-react";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@onicode/client-runtime/state/runtime";
import type { AgentSignInRequest, AuthMcpClientAccess, EnvironmentId } from "@onicode/contracts";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import { AsyncResult, Atom } from "effect/reactivity";
import { T, useGT, useMessages, Var } from "gt-react-native";
import { useState } from "react";
import { ScrollView, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { AppText as Text } from "../../components/ui/AppText";
import { Button } from "../../components/ui/Button";
import { Sheet } from "../../components/ui/Sheet";
import { useNowSecond } from "../../lib/clock";
import { connectedEnvironmentIdsAtom, environmentPresentations } from "../../state/presentation";
import { serverEnvironment } from "../../state/server";
import { useAtomCommand } from "../../state/use-atom-command";
import { AgentAccessChoices, useGrantableAgentAccess } from "./AgentAccessChoices";
import { closestGrantable, DEFAULT_AGENT_ACCESS, formatCountdown } from "./agentAccess.logic";

interface WaitingSignIn {
  readonly environmentId: EnvironmentId;
  readonly environmentLabel: string;
  readonly request: AgentSignInRequest;
}

const sameQueue = (a: ReadonlyArray<WaitingSignIn>, b: ReadonlyArray<WaitingSignIn>) =>
  a.length === b.length &&
  a.every(
    (item, index) =>
      item.request === b[index]?.request && item.environmentLabel === b[index]?.environmentLabel,
  );

/**
 * Every outside agent's sign-in waiting on a connected environment this device may manage access
 * on, oldest environment first. Only servers that publish agent sign-ins are subscribed.
 */
const waitingSignInsAtom = Atom.make((get): ReadonlyArray<WaitingSignIn> => {
  const next: WaitingSignIn[] = [];
  for (const environmentId of get(connectedEnvironmentIdsAtom)) {
    const presentation = get(environmentPresentations.presentationAtom(environmentId));
    if (presentation?.serverConfig?.environment.capabilities.webhookSchedules !== true) continue;
    if (!get(serverEnvironment.answerAgentSignIn.permissionAtom(environmentId))) continue;
    const requests = AsyncResult.getOrElse(
      get(serverEnvironment.agentSignIns({ environmentId, input: {} })),
      () => [],
    );
    for (const request of requests) {
      next.push({ environmentId, environmentLabel: presentation.entry.target.label, request });
    }
  }
  const previous = Option.getOrNull(get.self<ReadonlyArray<WaitingSignIn>>());
  return previous !== null && sameQueue(previous, next) ? previous : next;
}).pipe(Atom.withLabel("mobile-agent-sign-ins"));

const signInKey = (item: WaitingSignIn) => `${item.environmentId}:${item.request.id}`;

/**
 * Asks to approve an outside agent's sign-in, one at a time across environments. Any device can
 * answer; the sheet closes here when another one does.
 */
export function AgentSignInHost() {
  const waiting = useAtomValue(waitingSignInsAtom);
  // Closing without an answer leaves the sign-in to another device or its expiry.
  const [setAside, setSetAside] = useState<ReadonlySet<string>>(() => new Set());
  const open = waiting.filter((item) => !setAside.has(signInKey(item)));
  const current = open[0];
  if (current === undefined) return null;
  return (
    <AgentSignInSheet
      key={signInKey(current)}
      item={current}
      queued={open.length - 1}
      onSetAside={() => setSetAside((keys) => new Set(keys).add(signInKey(current)))}
    />
  );
}

function AgentSignInSheet(props: {
  readonly item: WaitingSignIn;
  readonly queued: number;
  readonly onSetAside: () => void;
}) {
  const gt = useGT();
  const m = useMessages();
  const insets = useSafeAreaInsets();
  const { environmentId, environmentLabel, request } = props.item;
  const denied = useGrantableAgentAccess(environmentId, environmentLabel);
  const [picked, setPicked] = useState<AuthMcpClientAccess | null>(null);
  const access = picked ?? closestGrantable(DEFAULT_AGENT_ACCESS, denied);
  const [pending, setPending] = useState<"approve" | "deny" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const answer = useAtomCommand(serverEnvironment.answerAgentSignIn, {
    label: "answer agent sign-in",
    reportFailure: false,
  });
  const now = useNowSecond();
  const left = Math.max(0, Math.floor((DateTime.toEpochMillis(request.expiresAt) - now) / 1000));
  const time = formatCountdown(left);

  const respond = async (decision: "approve" | "deny") => {
    if (pending !== null || (decision === "approve" && access === null)) return;
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
    // On success the request leaves the stream and this sheet unmounts.
    if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
      setError(agentSignInFailureMessage(squashAtomCommandFailure(result), m));
    }
  };

  return (
    <Sheet
      title={gt("Agent sign-in")}
      onClose={props.onSetAside}
      // An answer that lands after a close would still apply, and would close the next sheet.
      dismissDisabled={pending !== null}
      // Android lays the content out at the tall detent, so a half sheet hides the answers.
      opensExpanded
    >
      <View className="flex-1 bg-sheet-solid">
        <ScrollView
          nestedScrollEnabled
          className="flex-1"
          contentInsetAdjustmentBehavior="automatic"
          contentContainerClassName="gap-5 p-5"
        >
          <View className="gap-1">
            <Text className="font-oni-bold text-lg text-foreground">
              <T>
                Allow <Var>{request.clientName}</Var> to use Onicode?
              </T>
            </Text>
            <Text className="text-sm leading-5 text-foreground-muted">
              <T>
                An app is signing in to <Var>{environmentLabel}</Var>. Approve only if its sign-in
                page shows this code.
              </T>
            </Text>
          </View>
          <Text
            selectable
            accessibilityLabel={gt("Match code {code}", { code: request.matchCode })}
            className="text-center font-mono text-3xl font-oni-bold tracking-widest text-foreground"
          >
            {request.matchCode}
          </Text>
          {request.loopbackRedirect ? (
            <Text className="text-sm leading-5 text-foreground-muted">
              <T>It returns to an app on the computer where the sign-in page opened.</T>
            </Text>
          ) : (
            <View
              accessibilityRole="alert"
              className="rounded-xl border border-warning-border bg-warning p-3"
            >
              <Text className="text-sm leading-snug text-warning-foreground">
                <T>
                  It returns to <Var>{request.redirectHost}</Var>, which gets this access. Approve
                  only if you started this sign-in there.
                </T>
              </Text>
            </View>
          )}
          {access === null ? (
            <Text className="text-sm text-danger-foreground">
              {denied.get(DEFAULT_AGENT_ACCESS) ?? gt("This device cannot grant any access.")}
            </Text>
          ) : (
            <AgentAccessChoices
              title={gt("What it may do")}
              value={access}
              denied={denied}
              disabled={pending !== null}
              onChange={setPicked}
            />
          )}
          <Text className="text-xs text-foreground-muted">
            {props.queued > 0
              ? gt("Expires in {time} · {count} more waiting", { time, count: props.queued })
              : gt("Expires in {time}", { time })}
          </Text>
          {error !== null ? (
            <Text
              accessibilityRole="alert"
              accessibilityLiveRegion="polite"
              className="text-sm text-danger-foreground"
            >
              {error}
            </Text>
          ) : null}
        </ScrollView>
        <View
          className="gap-2 border-t border-border-subtle px-5 pt-3"
          style={{ paddingBottom: insets.bottom + 12 }}
        >
          <Button
            label={pending === "approve" ? gt("Approving…") : gt("Approve")}
            tone="primary"
            fullWidth
            disabled={pending !== null || access === null}
            loading={pending === "approve"}
            onPress={() => void respond("approve")}
          />
          <Button
            label={pending === "deny" ? gt("Denying…") : gt("Deny")}
            tone="secondary"
            fullWidth
            disabled={pending !== null}
            loading={pending === "deny"}
            onPress={() => void respond("deny")}
          />
        </View>
      </View>
    </Sheet>
  );
}
