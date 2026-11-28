import { agentSignInFailureMessage } from "@onicode/client-runtime/agent-sign-in-failure";
import {
  publicAccessStatusText,
  publicAccessSwitchDenial,
} from "@onicode/client-runtime/public-access-status";
import { useAtomValue } from "@effect/atom-react";
import { connectionRouteAddress, connectionRoutes } from "@onicode/client-runtime/connection";
import { errorDisplayMessage } from "@onicode/client-runtime/errors";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@onicode/client-runtime/state/runtime";
import {
  AuthAccessReadScope,
  AuthAccessWriteScope,
  type AuthClientSession,
  type AuthMcpClientAccess,
  type EnvironmentId,
  mcpApprovalCode,
} from "@onicode/contracts";
import type { StaticScreenProps } from "@react-navigation/native";
import * as DateTime from "effect/DateTime";
import * as Redacted from "effect/Redacted";
import { msg, T, useGT, useMessages } from "gt-react-native";
import { useState } from "react";
import { View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { SymbolView } from "../../components/ui/AppSymbol";
import * as Dialog from "../../components/ui/Dialog";
import { AppText as Text, AppTextInput } from "../../components/ui/AppText";
import { Button } from "../../components/ui/Button";
import { ListActionRow } from "../../components/ui/ListActionRow";
import { RowPressable } from "../../components/ui/RowPressable";
import { ScreenScrollView } from "../../components/ui/ScreenScrollView";
import { environmentCatalog } from "../../connection/catalog";
import { cn } from "../../lib/cn";
import { useDeadlinePassed, useNowSecond } from "../../lib/clock";
import { tryCopyTextWithHaptic } from "../../lib/copyTextWithHaptic";
import { tryOpenExternalUrl } from "../../lib/openExternalUrl";
import { authEnvironment } from "../../state/auth";
import { useEnvironmentPresentation } from "../../state/presentation";
import { serverEnvironment } from "../../state/server";
import { useEnvironmentScope, usePreparedConnection } from "../../state/session";
import { useAtomCommand } from "../../state/use-atom-command";
import { AgentAccessChoices, useGrantableAgentAccess } from "../agent-access/AgentAccessChoices";
import {
  agentSessionAccess,
  agentSessionName,
  claudeCodeCommands,
  closestGrantable,
  codexCommands,
  DEFAULT_AGENT_ACCESS,
  deviceMcpUrl,
  tokenMcpUrl,
  formatCountdown,
  outsideAgentSessions,
} from "../agent-access/agentAccess.logic";
import { usePublicAccess } from "../agent-access/usePublicAccess";
import { formatMessageTimestamp } from "../threads/messageTimestamp";
import { useEnvironmentQuery } from "../../state/query";
import { SettingsScreen } from "./components/SettingsScreen";
import { SettingsSection } from "./components/SettingsSection";
import { usePublicAccessWriter } from "./PublicAccessSection";
import { LabeledConnectionField } from "../connection/ConnectionFormField";

const DEVICE_APPS = [
  { name: "Claude Code", commands: claudeCodeCommands },
  { name: "Codex", commands: codexCommands },
] as const;

const CLOUD_APPS = [
  {
    name: "Claude",
    link: (url: string) =>
      `https://claude.ai/customize/connectors?modal=add-custom-connector&connectorName=Onicode&connectorUrl=${encodeURIComponent(url)}`,
    steps: [
      msg("In Claude, open Customize, then Connectors."),
      msg("Choose Add custom connector and paste the URL."),
      msg("The Free plan allows one custom connector."),
    ],
  },
  {
    name: "ChatGPT",
    link: () => "https://chatgpt.com/plugins",
    steps: [
      msg("Open chatgpt.com/plugins on the web, then + and Add custom MCP server."),
      msg("Paste the URL. Choose OAuth."),
      msg("Create it as a plugin, then install it."),
    ],
  },
] as const;

export function SettingsEnvironmentMcpRouteScreen({
  route,
}: StaticScreenProps<{
  readonly environmentId: EnvironmentId;
}>) {
  return (
    <UseInAnotherApp key={route.params.environmentId} environmentId={route.params.environmentId} />
  );
}

function UseInAnotherApp({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const gt = useGT();
  const insets = useSafeAreaInsets();
  const { presentation } = useEnvironmentPresentation(environmentId);
  const label = presentation?.entry.target.label ?? "Onicode";
  const connected = presentation?.connection.phase === "connected";
  const supported = presentation?.serverConfig?.environment.capabilities.webhookSchedules === true;
  return (
    <SettingsScreen title={gt("Use in another app")}>
      <ScreenScrollView
        contentInsetAdjustmentBehavior="automatic"
        className="flex-1"
        contentContainerClassName="gap-6 px-5 pt-4"
        contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 18) + 18 }}
        keyboardShouldPersistTaps="handled"
      >
        {!connected ? (
          <Text className="px-2 text-base text-foreground-muted">
            <T>Connect to the environment first.</T>
          </Text>
        ) : !supported ? (
          <Text className="px-2 text-base text-foreground-muted">
            <T>Update Onicode on this environment to use it from other apps.</T>
          </Text>
        ) : (
          <McpSetup environmentId={environmentId} environmentLabel={label} />
        )}
      </ScreenScrollView>
    </SettingsScreen>
  );
}

function McpSetup({
  environmentId,
  environmentLabel,
}: {
  readonly environmentId: EnvironmentId;
  readonly environmentLabel: string;
}) {
  const gt = useGT();
  const prepared = usePreparedConnection(environmentId);
  const entry = useAtomValue(environmentCatalog.catalogValueAtom, (catalog) =>
    catalog.entries.get(environmentId),
  );
  const publicAccess = usePublicAccess(environmentId);
  const addresses = [
    prepared._tag === "Some" ? prepared.value.httpBaseUrl : null,
    ...(entry === undefined ? [] : connectionRoutes(entry).map(connectionRouteAddress)),
  ];
  const agentUrl = deviceMcpUrl(addresses) ?? publicAccess.mcpUrl;
  const denied = useGrantableAgentAccess(environmentId, environmentLabel);
  const [picked, setPicked] = useState<AuthMcpClientAccess | null>(null);
  const access = picked ?? closestGrantable(DEFAULT_AGENT_ACCESS, denied);
  return (
    <>
      <Text className="px-2 text-sm leading-5 text-foreground-muted">
        <T>
          Apps such as Claude Code, Codex, Claude and ChatGPT use Onicode over MCP. When one signs
          in, approve it in Onicode on any of your devices, and check that both show the same code.
        </T>
      </Text>
      <SettingsSection
        title={gt("Apps on your computers")}
        footer={agentUrl === null ? undefined : gt("Run these where the app runs.")}
      >
        {agentUrl === null ? (
          <Text className="px-4 py-3 text-sm text-foreground-muted">
            <T>
              Agents finish signing in only over https or on the computer running Onicode. Turn on
              Tailscale HTTPS in Settings → Sharing & access on that computer, or turn on public
              agents.
            </T>
          </Text>
        ) : (
          <>
            <CopyRow label={gt("URL")} value={agentUrl} />
            {DEVICE_APPS.map((app) => (
              <View key={app.name}>
                <Text
                  textRole="footnote"
                  className="border-t border-border-subtle px-4 pt-3 font-oni-medium text-foreground"
                >
                  {app.name}
                </Text>
                {app.commands(agentUrl).map((command) => (
                  <CopyRow key={command} label={gt("Command")} value={command} />
                ))}
              </View>
            ))}
          </>
        )}
      </SettingsSection>
      <CloudApps environmentId={environmentId} publicAccess={publicAccess} />
      {access === null ? (
        <Text className="px-2 text-sm text-foreground-muted">
          {denied.get(DEFAULT_AGENT_ACCESS) ?? gt("This device cannot grant any access.")}
        </Text>
      ) : (
        <>
          <AgentAccessChoices
            title={gt("What it may do")}
            footer={gt("Applies to the code and the token you create below.")}
            value={access}
            denied={denied}
            onChange={setPicked}
          />
          <AgentCode
            key={access}
            environmentId={environmentId}
            environmentLabel={environmentLabel}
            access={access}
          />
          <AgentToken
            environmentId={environmentId}
            access={access}
            mcpUrl={agentUrl ?? tokenMcpUrl(addresses)}
          />
        </>
      )}
      <ConnectedAgents environmentId={environmentId} />
    </>
  );
}

/** Cloud apps call from the internet, so they reach only the public address. */
function CloudApps({
  environmentId,
  publicAccess,
}: {
  readonly environmentId: EnvironmentId;
  readonly publicAccess: ReturnType<typeof usePublicAccess>;
}) {
  const gt = useGT();
  const m = useMessages();
  const { agents, status, mcpUrl } = publicAccess;
  const canChange = useEnvironmentScope(environmentId, AuthAccessWriteScope);
  const { presentation } = useEnvironmentPresentation(environmentId);
  const denial = publicAccessSwitchDenial(
    presentation?.serverConfig?.managedPolicy,
    "publicAgentsEnabled",
    false,
    m,
  );
  const { write, pending, error } = usePublicAccessWriter(environmentId);
  const statusText = status === null ? null : publicAccessStatusText(status, m);
  if (mcpUrl !== null) {
    return (
      <SettingsSection title={gt("Cloud apps")}>
        <CopyRow label={gt("Public URL")} value={mcpUrl} />
        {CLOUD_APPS.map((app) => (
          <View key={app.name} className="border-t border-border-subtle">
            <View className="gap-1 px-4 pt-3">
              <Text textRole="body" className="text-foreground">
                {app.name}
              </Text>
              {app.steps.map((step, index) => (
                <View key={step} className="flex-row gap-2">
                  <Text textRole="footnote" className="text-foreground-muted">
                    {index + 1}.
                  </Text>
                  <Text textRole="footnote" className="min-w-0 flex-1 text-foreground-muted">
                    {m(step)}
                  </Text>
                </View>
              ))}
            </View>
            <ListActionRow
              icon="arrow.up.right"
              label={gt("Open {app}", { app: app.name })}
              onPress={() => void tryOpenExternalUrl(app.link(mcpUrl))}
            />
          </View>
        ))}
      </SettingsSection>
    );
  }
  return (
    <SettingsSection title={gt("Cloud apps")}>
      <View className="gap-3 p-4">
        <Text className="text-sm leading-5 text-foreground-muted">
          <T>
            Cloud apps, such as Claude and ChatGPT, reach Onicode from the internet. Public agents
            publishes only /mcp and its sign-in through Tailscale Funnel; every app still needs your
            approval. The tailnet needs HTTPS certificates and the funnel node attribute.
          </T>
        </Text>
        {agents ? null : denial !== undefined ? (
          <Text className="text-sm text-foreground-muted">{denial}</Text>
        ) : canChange ? (
          <Button
            label={gt("Turn on public agents")}
            tone="primary"
            fullWidth
            loading={pending}
            disabled={pending}
            onPress={() => void write({ publicAgentsEnabled: true })}
          />
        ) : (
          <Text className="text-sm text-foreground-muted">
            <T>This device cannot change this environment's settings.</T>
          </Text>
        )}
        {statusText !== null ? (
          <Text
            selectable
            className={
              status?._tag === "failed"
                ? "text-sm text-danger-foreground"
                : "text-sm text-foreground-muted"
            }
          >
            {statusText}
          </Text>
        ) : null}
        {error !== null ? (
          <Text selectable accessibilityRole="alert" className="text-sm text-danger-foreground">
            {error}
          </Text>
        ) : null}
      </View>
    </SettingsSection>
  );
}

/** "Get code" mints on press; a code is one use and lives five minutes. */
function AgentCode({
  environmentId,
  environmentLabel,
  access,
}: {
  readonly environmentId: EnvironmentId;
  readonly environmentLabel: string;
  readonly access: AuthMcpClientAccess;
}) {
  const gt = useGT();
  const [code, setCode] = useState<{ readonly value: string; readonly expiresAt: number } | null>(
    null,
  );
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mint = useAtomCommand(authEnvironment.createPairingCredential, {
    label: "agent code",
    // The failure cause holds the request; keep it out of the console.
    reportFailure: false,
  });
  const expired = useDeadlinePassed(code?.expiresAt ?? null);
  const live = code !== null && !expired;
  const getCode = async () => {
    setPending(true);
    setError(null);
    const result = await mint({
      environmentId,
      input: { access, purpose: "agent", label: gt("Agent code") },
    });
    setPending(false);
    if (result._tag === "Success") {
      setCode({
        value: mcpApprovalCode(access, result.value.credential),
        expiresAt: DateTime.toEpochMillis(result.value.expiresAt),
      });
    } else {
      setError(
        gt("Could not get a code from {environment}. Try again.", {
          environment: environmentLabel,
        }),
      );
    }
  };
  return (
    <SettingsSection
      title={gt("Approve with a code")}
      footer={gt(
        "No Onicode device at hand? Enter the code on the app's sign-in page. It sets what the app may do.",
      )}
    >
      {live ? (
        <>
          <CopyRow label={gt("Code", { $context: "approval code" })} value={code.value} />
          <CodeExpiry expiresAt={code.expiresAt} />
        </>
      ) : (
        <ListActionRow
          icon="qrcode.viewfinder"
          label={code === null ? gt("Get code") : gt("New code")}
          loading={pending}
          onPress={() => void getCode()}
        />
      )}
      {error !== null ? (
        <Text accessibilityRole="alert" className="px-4 pb-3 text-sm text-danger-foreground">
          {error}
        </Text>
      ) : null}
    </SettingsSection>
  );
}

function CodeExpiry({ expiresAt }: { readonly expiresAt: number }) {
  const gt = useGT();
  const now = useNowSecond();
  const left = Math.max(0, Math.floor((expiresAt - now) / 1000));
  return (
    <Text className="px-4 pb-3 text-xs text-foreground-muted">
      {gt("Expires in {time} · one use", { time: formatCountdown(left) })}
    </Text>
  );
}

/** For apps that send a fixed Authorization header instead of signing in. */
function AgentToken({
  environmentId,
  access,
  mcpUrl,
}: {
  readonly environmentId: EnvironmentId;
  readonly access: AuthMcpClientAccess;
  readonly mcpUrl: string | null;
}) {
  const gt = useGT();
  const m = useMessages();
  const [name, setName] = useState("");
  const [token, setToken] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const issue = useAtomCommand(serverEnvironment.issueAgentToken, {
    label: "agent token",
    reportFailure: false,
  });
  const create = async () => {
    const label = name.trim();
    if (pending || label === "") return;
    setPending(true);
    setError(null);
    const result = await issue({ environmentId, input: { label, access } });
    setPending(false);
    if (result._tag === "Success") setToken(Redacted.value(result.value.token));
    else setError(agentSignInFailureMessage(squashAtomCommandFailure(result), m));
  };
  return (
    <SettingsSection
      title={gt("Create a token")}
      footer={
        token === null
          ? gt(
              "For apps without sign-in. The app sends it as a bearer token. It works for 30 days, until you revoke it under Connected agents.",
            )
          : gt("Copy it now; Onicode does not show it again.")
      }
    >
      {token === null ? (
        <View className="gap-3 p-4">
          <LabeledConnectionField label={gt("Token name")}>
            <AppTextInput
              accessibilityLabel={gt("Token name")}
              placeholder={gt("Name, such as Nightly triage")}
              className="min-h-11 rounded-xl border-continuous bg-card px-3 text-base text-foreground"
              value={name}
              onChangeText={setName}
              onSubmitEditing={() => void create()}
              returnKeyType="done"
              editable={!pending}
            />
          </LabeledConnectionField>
          <Button
            label={pending ? gt("Creating…") : gt("Create token")}
            tone="secondary"
            fullWidth
            loading={pending}
            disabled={pending || name.trim() === ""}
            onPress={() => void create()}
          />
          {error !== null ? (
            <Text accessibilityRole="alert" className="text-sm text-danger-foreground">
              {error}
            </Text>
          ) : null}
        </View>
      ) : (
        <>
          <CopyRow label={gt("Token")} value={token} />
          <CopyRow label={gt("Header")} value={`Authorization: Bearer ${token}`} separated />
          {mcpUrl === null ? null : <CopyRow label={gt("URL")} value={mcpUrl} separated />}
        </>
      )}
    </SettingsSection>
  );
}

/** Outside agents that signed in or hold a token here, so access given here can be taken back. */
function ConnectedAgents({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const gt = useGT();
  const m = useMessages();
  const canRead = useEnvironmentScope(environmentId, AuthAccessReadScope);
  const canRevoke = useEnvironmentScope(environmentId, AuthAccessWriteScope);
  const access = useEnvironmentQuery(
    canRead ? authEnvironment.accessChanges({ environmentId, input: null }) : null,
  );
  const revokeClient = useAtomCommand(authEnvironment.revokeClient, {
    label: "revoke outside agent",
    reportFailure: false,
  });
  const [revoking, setRevoking] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  if (!canRead) return null;
  const snapshot = access.data?.type === "snapshot" ? access.data.payload : null;
  const agents = outsideAgentSessions(snapshot?.clientSessions ?? []);
  const revoke = async (session: AuthClientSession) => {
    setRevoking(session.sessionId);
    setError(null);
    const result = await revokeClient({ environmentId, input: { sessionId: session.sessionId } });
    setRevoking(null);
    if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
      const cause = squashAtomCommandFailure(result);
      setError(
        (cause instanceof Error ? errorDisplayMessage(cause, m) : null) ??
          gt("Could not revoke the agent's access. Try again."),
      );
    }
  };
  const confirmRevoke = (session: AuthClientSession, label: string) =>
    Dialog.alert(
      gt("Revoke {agent}?", { agent: label }),
      gt("It loses access to this environment at once. To use it again, sign it in again."),
      [
        { text: gt("Cancel"), style: "cancel" },
        {
          text: gt("Revoke", { $context: "verb, removes an agent's access" }),
          style: "destructive",
          onPress: () => void revoke(session),
        },
      ],
    );
  return (
    <SettingsSection title={gt("Connected agents")}>
      {access.error !== null ? (
        <Text selectable className="px-4 py-3 text-sm text-danger-foreground">
          {access.error}
        </Text>
      ) : agents.length === 0 ? (
        <Text className="px-4 py-3 text-sm text-foreground-muted">
          {snapshot === null
            ? gt("Loading agents…")
            : gt("No outside agents use this environment.")}
        </Text>
      ) : (
        agents.map((session, index) => {
          const label = agentSessionName(session) ?? gt("Agent");
          return (
            <ConnectedAgentRow
              key={session.sessionId}
              session={session}
              label={label}
              separated={index > 0}
              revoking={revoking === session.sessionId}
              canRevoke={canRevoke && revoking === null}
              onRevoke={() => confirmRevoke(session, label)}
            />
          );
        })
      )}
      {error !== null ? (
        <Text accessibilityRole="alert" className="px-4 pb-3 text-sm text-danger-foreground">
          {error}
        </Text>
      ) : null}
    </SettingsSection>
  );
}

function ConnectedAgentRow(props: {
  readonly session: AuthClientSession;
  readonly label: string;
  readonly separated: boolean;
  readonly revoking: boolean;
  readonly canRevoke: boolean;
  readonly onRevoke: () => void;
}) {
  const gt = useGT();
  const { session } = props;
  const live = session.connected;
  const status = live
    ? gt("Connected", { $context: "connection status" })
    : // Calls to /mcp do not record a connection, so an agent in use may have none.
      session.lastConnectedAt === null
      ? gt("Added {time}", { time: formatMessageTimestamp(DateTime.formatIso(session.issuedAt)) })
      : gt("Last connected {time}", {
          time: formatMessageTimestamp(DateTime.formatIso(session.lastConnectedAt)),
        });
  // Older clients read the few scopes they know; the whole grant is in permissions.
  const scopes = session.permissions ?? session.scopes;
  const level = agentSessionAccess(scopes);
  const accessText =
    level === "read-only"
      ? gt("Read only")
      : level === "full-access"
        ? gt("Full access")
        : gt("{count, plural, one {{count} scope} other {{count} scopes}}", {
            count: scopes.length,
          });
  return (
    <View
      className={cn(
        "flex-row items-center gap-3 px-4 py-3",
        props.separated && "border-t border-border-subtle",
      )}
    >
      <View
        accessibilityElementsHidden
        importantForAccessibility="no-hide-descendants"
        className={cn(
          "size-2 rounded-full",
          live ? "bg-adaptive-emerald-600-400" : "bg-subtle-strong",
        )}
      />
      <View className="min-w-0 flex-1 gap-1">
        <Text textRole="body" className="text-foreground" numberOfLines={1}>
          {props.label}
        </Text>
        <Text textRole="footnote" className="text-foreground-muted">
          {`${status} · ${accessText}`}
        </Text>
      </View>
      <Button
        label={
          props.revoking
            ? gt("Revoking…")
            : gt("Revoke", { $context: "verb, removes an agent's access" })
        }
        accessibilityLabel={gt("Revoke {agent}", { agent: props.label })}
        tone="danger"
        loading={props.revoking}
        disabled={!props.canRevoke || props.revoking}
        onPress={props.onRevoke}
      />
    </View>
  );
}

function CopyRow({
  label,
  value,
  separated = false,
}: {
  readonly label: string;
  readonly value: string;
  readonly separated?: boolean;
}) {
  const gt = useGT();
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    if (!(await tryCopyTextWithHaptic(value, { target: label }))) return;
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };
  return (
    <RowPressable
      accessibilityRole="button"
      accessibilityLabel={gt("Copy {label}", { label })}
      accessibilityHint={value}
      onPress={() => void copy()}
      className={cn("bg-grouped-card", separated && "border-t border-border-subtle")}
    >
      <View className="flex-row items-center gap-3 px-4 py-3">
        <View className="min-w-0 flex-1 gap-1">
          <Text textRole="footnote" className="text-foreground-muted">
            {label}
          </Text>
          <Text className="font-mono text-sm text-foreground">{value}</Text>
        </View>
        <SymbolView
          name={copied ? "checkmark" : "doc.on.doc"}
          size={18}
          tintColorClassName="accent-icon"
          type="monochrome"
        />
      </View>
    </RowPressable>
  );
}
