import type { OrchestrationV2UserMessageInputIntent } from "@onicode/contracts";
import { msg } from "gt-react-native";

import { m, type MessageResolver } from "../../i18n/messages";

export interface UserMessageIntentBadgePresentation {
  readonly label: string;
  readonly accessibilityLabel: string;
  /** An unread steer waits for its turn; "sending" once the user asked to send it now. */
  readonly state: "read" | "waiting" | "sending";
}

export function resolveUserMessageIntentBadge(
  intent: OrchestrationV2UserMessageInputIntent | undefined,
  unread = false,
  sentNow = false,
  resolve: MessageResolver = m,
): UserMessageIntentBadgePresentation | null {
  if (unread && (intent === "steer" || intent === "promoted_queued_to_steer")) {
    const label = resolve(msg("Steer", { $context: "badge, message sent into the running turn" }));
    return sentNow
      ? {
          label,
          accessibilityLabel: resolve(msg("Steer being sent to the agent now")),
          state: "sending",
        }
      : {
          label,
          accessibilityLabel: resolve(
            msg("Steer waiting for the agent to read it after the running command finishes"),
          ),
          state: "waiting",
        };
  }
  switch (intent) {
    case "steer":
      return {
        label: resolve(msg("Steer", { $context: "badge, message sent into the running turn" })),
        accessibilityLabel: resolve(msg("Steered the active turn")),
        state: "read",
      };
    case "promoted_queued_to_steer":
      return {
        label: resolve(msg("Steer", { $context: "badge, message sent into the running turn" })),
        accessibilityLabel: resolve(
          msg("Originally queued, then promoted to steer the active turn"),
        ),
        state: "read",
      };
    case "queued_turn":
    case "turn_start":
    case undefined:
      return null;
  }
}
