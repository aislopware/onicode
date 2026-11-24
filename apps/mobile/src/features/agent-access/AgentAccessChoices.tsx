import { useAtomValue } from "@effect/atom-react";
import {
  AuthAccessWriteScope,
  type AuthMcpClientAccess,
  type EnvironmentId,
} from "@onicode/contracts";
import { useMessages } from "gt-react-native";

import { ListChoiceRow } from "../../components/ui/ListChoiceRow";
import { environmentPresentations } from "../../state/presentation";
import { environmentSession, useEnvironmentScope } from "../../state/session";
import { SettingsSection } from "../settings/components/SettingsSection";
import { LINK_ACCESS_CHOICES } from "../threads/LinkRequestCard";
import { agentAccessDenials } from "./agentAccess.logic";

/** Why this device cannot grant each level on `environmentId`; absent when it can. */
export function useGrantableAgentAccess(environmentId: EnvironmentId, environmentLabel: string) {
  const session = useAtomValue(environmentSession.sessionStateValueAtom(environmentId));
  const canGrant = useEnvironmentScope(environmentId, AuthAccessWriteScope);
  const policy = useAtomValue(
    environmentPresentations.presentationAtom(environmentId),
    (presentation) => presentation?.serverConfig?.managedPolicy,
  );
  const resolve = useMessages();
  return agentAccessDenials({ session, canGrant, policy, environmentLabel }, resolve);
}

/** The five access levels as choices; a level this device cannot grant says why. */
export function AgentAccessChoices(props: {
  readonly title: string;
  readonly footer?: string;
  readonly value: AuthMcpClientAccess;
  readonly denied: ReadonlyMap<AuthMcpClientAccess, string>;
  readonly disabled?: boolean;
  readonly onChange: (access: AuthMcpClientAccess) => void;
}) {
  const m = useMessages();
  return (
    <SettingsSection title={props.title} footer={props.footer}>
      {LINK_ACCESS_CHOICES.map((choice, index) => {
        const reason = props.denied.get(choice.access);
        return (
          <ListChoiceRow
            key={choice.access}
            label={m(choice.label)}
            description={reason ?? m(choice.description)}
            selected={choice.access === props.value}
            separated={index > 0}
            disabled={props.disabled === true || reason !== undefined}
            onPress={() => props.onChange(choice.access)}
          />
        );
      })}
    </SettingsSection>
  );
}
