import { pipe } from "effect/Function";
import {
  SECRET_REQUEST_DEFAULT_PLACEHOLDER,
  secretRequestAnswerInput,
  secretRequestDestination,
  secretRequestDisplay,
  secretRequestFailureMessage,
  secretRequestLink,
  SECRET_REQUEST_PRIVACY_NOTE,
  secretRequestReference,
  secretRequestSources,
  type SecretRequestItem,
  type SecretRequestReply,
} from "@onicode/client-runtime/secret-request";
import { scopeProjectRef, scopeThreadRef } from "@onicode/client-runtime/environment";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@onicode/client-runtime/state/runtime";
import {
  resolveEnvironmentMachineKind,
  type EnvironmentId,
  type OrchestrationV2ProjectedTurnItem,
  type SecretProviderId,
} from "@onicode/contracts";
import { useAtomValue } from "@effect/atom-react";
import { useRef, useState } from "react";
import { Pressable, View, type ColorValue } from "react-native";

import { EnvironmentMachineSymbol } from "../../components/EnvironmentMachineSymbol";
import { SymbolView, type AppSymbolName } from "../../components/ui/AppSymbol";
import { AppText as Text, AppTextInput as TextInput } from "../../components/ui/AppText";
import { SecretTextInput } from "../../components/ui/SecretTextInput";
import { useProject, useQuietThreadShell } from "../../state/entities";
import { serverEnvironment } from "../../state/server";
import { useAtomCommand } from "../../state/use-atom-command";
import { tryOpenExternalUrl } from "../../lib/openExternalUrl";
import { Button } from "../../components/ui/Button";
import { Chip } from "../../components/ui/Chip";

const LOCK_SYMBOL: AppSymbolName = { ios: "lock", android: "lock" };
const PRIVATE_SYMBOL: AppSymbolName = { ios: "checkmark.shield", android: "lock" };

export function SecretRequestCard(props: {
  readonly environmentId: EnvironmentId;
  readonly projectedItem: OrchestrationV2ProjectedTurnItem;
  readonly iconColor: ColorValue;
}) {
  const { item, visibility } = props.projectedItem;
  if (item.type !== "secret_request") return null;
  const display = secretRequestDisplay(item, visibility);
  if (display.kind === "pending") {
    return (
      <PendingSecretRequestForm
        environmentId={props.environmentId}
        item={item}
        iconColor={props.iconColor}
      />
    );
  }
  const icon: AppSymbolName =
    display.kind === "pending-elsewhere"
      ? LOCK_SYMBOL
      : display.outcome === "saved"
        ? "checkmark"
        : "minus";
  return (
    <View className="mb-3 min-h-9 flex-row items-center gap-2 px-1">
      <SymbolView name={icon} size={13} tintColor={props.iconColor} type="monochrome" />
      <Text className="flex-1 font-sans text-sm text-foreground-muted" numberOfLines={2}>
        {item.label} · {display.label}
      </Text>
    </View>
  );
}

function PendingSecretRequestForm(props: {
  readonly environmentId: EnvironmentId;
  readonly item: SecretRequestItem;
  readonly iconColor: ColorValue;
}) {
  const { item } = props;
  const answer = useAtomCommand(serverEnvironment.answerSecretRequest, {
    label: "answer secret request",
    // The failure cause holds the request; keep it out of the console.
    reportFailure: false,
    reportDefect: false,
  });
  const config = useAtomValue(serverEnvironment.configValueAtom(props.environmentId));
  const sources = secretRequestSources(config, config?.settings.secretProviders ?? {});
  const thread = useQuietThreadShell(scopeThreadRef(props.environmentId, item.threadId));
  const project = useProject(
    thread === null ? null : scopeProjectRef(props.environmentId, thread.projectId),
  );
  const destination = secretRequestDestination({
    environmentLabel: config?.environment.label,
    project,
  });
  const [sourceId, setSourceId] = useState<SecretProviderId | null>(null);
  const source = sources.find((candidate) => candidate.id === sourceId) ?? null;
  const [secret, setSecret] = useState("");
  const [reference, setReference] = useState("");
  const [sending, setSending] = useState<SecretRequestReply["type"] | null>(null);
  const submitting = sending !== null;
  const [error, setError] = useState<string | null>(null);
  // Submit then a tap can both run before a re-render; this guard is synchronous.
  const inFlight = useRef(false);

  const send = async (reply: SecretRequestReply) => {
    const input = secretRequestAnswerInput(item, reply);
    if (input === null || inFlight.current) return;
    inFlight.current = true;
    setSending(reply.type);
    setError(null);
    // iOS reads a password field as it leaves the screen and can pass the value on to be saved.
    // The answer removes this card, so the field is emptied before the answer is sent.
    const typed = reply.type === "save" ? reply.secret : secret;
    setSecret("");
    const result = await answer({ environmentId: props.environmentId, input }).finally(() => {
      inFlight.current = false;
      setSending(null);
    });
    if (result._tag === "Success") {
      setReference("");
      return;
    }
    setSecret(typed);
    if (!isAtomCommandInterrupted(result)) {
      pipe(squashAtomCommandFailure(result), secretRequestFailureMessage, setError);
    }
  };
  const save = () => {
    if (source === null) {
      void send({ type: "save", secret });
    } else if (secretRequestReference(source, reference) === undefined) {
      setError(source.invalidReferenceMessage);
    } else {
      void send({ type: "reference", source, reference });
    }
  };
  const draft = source === null ? secret : reference;
  const link = secretRequestLink(item);

  return (
    <View className="mb-3 gap-3 rounded-[20px] border border-border bg-card-alt p-4">
      <View className="gap-1">
        <Text className="font-oni-bold text-base text-foreground">{item.label}</Text>
        {item.reason.trim() ? (
          <Text className="font-sans text-sm leading-5 text-foreground-muted" selectable>
            {item.reason}
          </Text>
        ) : null}
      </View>
      {link !== null ? (
        <View className="gap-1.5">
          <Button
            label={`Open ${link.host}`}
            tone="secondary"
            fullWidth
            onPress={() => void tryOpenExternalUrl(link.href, "secret-request")}
          />
          {/* A link found in the reason is already shown there. */}
          {item.url !== undefined ? (
            <Text selectable className="font-mono text-xs text-foreground-muted">
              {item.url}
            </Text>
          ) : null}
        </View>
      ) : null}
      {sources.length > 0 ? (
        <View className="flex-row flex-wrap gap-2">
          <Chip
            label="Value"
            size="small"
            selected={source === null}
            disabled={submitting}
            onPress={() => {
              setSourceId(null);
              setError(null);
            }}
          />
          {sources.map((candidate) => (
            <Chip
              key={candidate.id}
              label={candidate.label}
              size="small"
              selected={candidate.id === source?.id}
              disabled={submitting}
              onPress={() => {
                setSourceId(candidate.id);
                setError(null);
              }}
            />
          ))}
        </View>
      ) : null}
      {source === null ? (
        <SecretTextInput
          accessibilityLabel={item.label}
          placeholder={
            sending === "save"
              ? "Saving…"
              : (item.placeholder ?? SECRET_REQUEST_DEFAULT_PLACEHOLDER)
          }
          value={secret}
          onChangeText={setSecret}
          editable={!submitting}
          onSubmitEditing={(value) => void send({ type: "save", secret: value })}
        />
      ) : (
        <TextInput
          key={source.id}
          accessibilityLabel={`${item.label} reference in ${source.label}`}
          placeholder={source.referencePlaceholder}
          value={reference}
          onChangeText={(next) => {
            setReference(next);
            setError(null);
          }}
          editable={!submitting}
          autoCorrect={false}
          autoCapitalize="none"
          autoComplete="off"
          textContentType="none"
          importantForAutofill="no"
          spellCheck={false}
          returnKeyType="done"
          onSubmitEditing={save}
        />
      )}
      {error !== null ? (
        <Text
          accessibilityRole="alert"
          accessibilityLiveRegion="polite"
          className="font-sans text-sm text-danger-foreground"
        >
          {error}
        </Text>
      ) : null}
      <Button
        label="Save securely"
        tone="primary"
        fullWidth
        disabled={submitting || draft.trim().length === 0}
        loading={sending === "save" || sending === "reference"}
        onPress={save}
      />
      {destination !== null ? (
        <View className="flex-row gap-1.5">
          <View
            className="h-4 justify-center"
            accessibilityElementsHidden
            importantForAccessibility="no-hide-descendants"
          >
            <EnvironmentMachineSymbol
              kind={resolveEnvironmentMachineKind(config)}
              size={13}
              tintColorClassName="accent-icon-subtle"
            />
          </View>
          <Text className="flex-1 font-sans text-xs text-foreground-muted">
            Goes to{" "}
            {destination.project !== null ? (
              <Text className="font-oni-medium text-xs text-foreground">{destination.project}</Text>
            ) : null}
            {destination.project !== null && destination.environment !== null ? " on " : null}
            {destination.environment !== null ? (
              <Text className="font-oni-medium text-xs text-foreground">
                {destination.environment}
              </Text>
            ) : null}
          </Text>
        </View>
      ) : null}
      <View className="flex-row items-end justify-between gap-3">
        <View className="flex-1 flex-row gap-1.5">
          <View className="h-4 justify-center">
            <SymbolView
              name={PRIVATE_SYMBOL}
              size={13}
              tintColor={props.iconColor}
              type="monochrome"
            />
          </View>
          <Text className="flex-1 font-sans text-xs text-foreground-muted">
            {SECRET_REQUEST_PRIVACY_NOTE}
          </Text>
        </View>
        <Pressable
          accessibilityRole="button"
          accessibilityState={{ disabled: submitting }}
          disabled={submitting}
          hitSlop={8}
          className="px-1 py-1 active:opacity-60 disabled:opacity-50"
          onPress={() => void send({ type: "decline" })}
        >
          <Text className="font-sans text-xs text-foreground-muted">Decline</Text>
        </Pressable>
      </View>
    </View>
  );
}
