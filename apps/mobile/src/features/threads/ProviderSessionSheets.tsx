import {
  contextCategoryDetail,
  contextUsageMcpTokensByServer,
  mcpServerCanAuthenticate,
} from "@onicode/client-runtime/provider-session-views";
import { squashAtomCommandFailure } from "@onicode/client-runtime/state/runtime";
import type {
  EnvironmentId,
  ProviderContextUsage,
  ProviderMcpServer,
  ProviderMcpServerAction,
  ThreadId,
} from "@onicode/contracts";
import { formatTokens } from "@onicode/shared/usageFormat";
import { StackActions, useNavigation, type StaticScreenProps } from "@react-navigation/native";
import { useEffect, useState } from "react";
import { Linking, Pressable, ScrollView, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { AppText as Text } from "../../components/ui/AppText";
import { Button } from "../../components/ui/Button";
import * as Dialog from "../../components/ui/Dialog";
import { ScreenContent } from "../../components/ui/ScreenContent";
import { SheetHeader } from "../../components/ui/SheetHeader";
import { ThemedSwitch } from "../../components/ui/ThemedSwitch";
import { cn } from "../../lib/cn";
import { providerSessionEnvironment } from "../../state/providerSession";
import { useEnvironmentQuery } from "../../state/query";
import { useAtomCommand } from "../../state/use-atom-command";

export type SessionTarget = { readonly environmentId: EnvironmentId; readonly threadId: ThreadId };

export function SheetBody(props: { readonly children: React.ReactNode }) {
  const insets = useSafeAreaInsets();
  return (
    <ScreenContent>
      <ScrollView
        nestedScrollEnabled
        className="flex-1"
        contentInsetAdjustmentBehavior="automatic"
        contentContainerClassName="gap-3 px-5 pb-6"
        contentContainerStyle={{ paddingBottom: Math.max(insets.bottom, 16) + 8 }}
      >
        {props.children}
      </ScrollView>
    </ScreenContent>
  );
}

export function Muted(props: {
  readonly children: React.ReactNode;
  readonly className?: string;
  readonly numberOfLines?: number;
}) {
  return (
    <Text
      textRole="footnote"
      numberOfLines={props.numberOfLines}
      className={cn("text-foreground-muted", props.className)}
    >
      {props.children}
    </Text>
  );
}

const USED_COLORS = [
  "bg-sky-500",
  "bg-violet-500",
  "bg-amber-500",
  "bg-emerald-500",
  "bg-rose-500",
  "bg-cyan-500",
  "bg-fuchsia-500",
  "bg-lime-500",
];

/** `/context`: what the thread's live agent session carries, by category. */
export function ThreadContextUsageSheet({ route }: StaticScreenProps<SessionTarget>) {
  const target = route.params;
  const navigation = useNavigation();
  const query = useEnvironmentQuery(
    providerSessionEnvironment.contextUsage({
      environmentId: target.environmentId,
      input: { threadId: target.threadId },
    }),
  );
  const usage = query.data;
  return (
    <View collapsable={false} className="flex-1 bg-sheet">
      <SheetHeader title="Context window" onClose={() => navigation.goBack()} />
      <SheetBody>
        {query.error !== null ? (
          <Text className="text-danger-foreground">{query.error}</Text>
        ) : usage === null ? (
          <Muted>Loading…</Muted>
        ) : (
          <ContextBreakdown
            usage={usage}
            onManageMcp={() => navigation.dispatch(StackActions.push("ThreadMcpServers", target))}
          />
        )}
        <Button label="Refresh" size="compact" disabled={query.isPending} onPress={query.refresh} />
      </SheetBody>
    </View>
  );
}

function ContextBreakdown(props: {
  readonly usage: ProviderContextUsage;
  readonly onManageMcp: () => void;
}) {
  const { usage } = props;
  const [expanded, setExpanded] = useState<string | null>(null);
  const categories = usage.categories.filter((category) => category.tokens > 0);
  const colorOf = (index: number) => {
    const category = categories[index]!;
    if (category.kind === "free") return "bg-subtle";
    if (category.kind !== "used") return "bg-subtle-strong";
    const usedBefore = categories.slice(0, index).filter((item) => item.kind === "used").length;
    return USED_COLORS[usedBefore % USED_COLORS.length]!;
  };
  const max = Math.max(usage.maxTokens, 1);
  return (
    <View className="gap-3">
      <Muted>
        {`${usage.model} · ${formatTokens(usage.totalTokens)} of ${formatTokens(usage.maxTokens)} tokens (${Math.round(usage.percentage)}%)`}
      </Muted>
      <View className="h-2 flex-row overflow-hidden rounded-full bg-subtle">
        {/* Deferred tools load on demand, so they take no room until used. */}
        {categories.map((category, index) =>
          category.kind === "deferred" ? null : (
            <View
              key={category.id ?? category.name}
              className={colorOf(index)}
              style={{ width: `${(category.tokens / max) * 100}%` }}
            />
          ),
        )}
      </View>
      <View>
        {categories.map((category, index) => {
          const key = category.id ?? category.name;
          const detail = contextCategoryDetail(category);
          const isOpen = expanded === key;
          const row = (
            <View className="flex-row items-center gap-2 py-2.5">
              <View className={cn("size-2.5 rounded-full", colorOf(index))} />
              <Text className="min-w-0 flex-1" numberOfLines={1}>
                {category.name}
              </Text>
              <Text className="tabular-nums text-foreground-muted">
                {category.kind === "deferred"
                  ? `${formatTokens(category.tokens)} on use`
                  : formatTokens(category.tokens)}
              </Text>
            </View>
          );
          return (
            <View key={key} className="border-b border-border">
              {detail === null ? (
                row
              ) : (
                <Pressable
                  accessibilityRole="button"
                  accessibilityState={{ expanded: isOpen }}
                  onPress={() => setExpanded(isOpen ? null : key)}
                  className="active:opacity-70"
                >
                  {row}
                </Pressable>
              )}
              {isOpen && detail !== null ? (
                <View className="gap-1 pb-3 ps-4">
                  <CategoryDetail detail={detail} usage={usage} onManageMcp={props.onManageMcp} />
                </View>
              ) : null}
            </View>
          );
        })}
      </View>
      <Muted>
        {usage.isAutoCompactEnabled
          ? usage.autoCompactThreshold !== undefined
            ? `Auto-compacts at ${formatTokens(usage.autoCompactThreshold)} tokens.`
            : "Auto-compact is on."
          : "Auto-compact is off."}
      </Muted>
    </View>
  );
}

function DetailRow(props: {
  readonly label: string;
  readonly hint?: string | undefined;
  readonly tokens: number;
}) {
  return (
    <View className="flex-row items-center gap-2">
      <Text textRole="footnote" className="min-w-0 flex-1" numberOfLines={1}>
        {props.label}
      </Text>
      {props.hint ? <Muted>{props.hint}</Muted> : null}
      <Muted className="tabular-nums">{formatTokens(props.tokens)}</Muted>
    </View>
  );
}

function CategoryDetail(props: {
  readonly detail: NonNullable<ReturnType<typeof contextCategoryDetail>>;
  readonly usage: ProviderContextUsage;
  readonly onManageMcp: () => void;
}) {
  const { usage } = props;
  const empty = <Muted>Nothing listed.</Muted>;
  switch (props.detail) {
    case "mcp": {
      const servers = contextUsageMcpTokensByServer(usage);
      return (
        <>
          {servers.length === 0
            ? empty
            : servers.map((server) => (
                <DetailRow
                  key={server.server}
                  label={server.server}
                  hint={`${server.tools} ${server.tools === 1 ? "tool" : "tools"}`}
                  tokens={server.tokens}
                />
              ))}
          <Button label="Manage MCP servers" size="compact" onPress={props.onManageMcp} />
        </>
      );
    }
    case "memory":
      return usage.memoryFiles.length === 0
        ? empty
        : usage.memoryFiles.map((file) => (
            <DetailRow key={file.path} label={file.path} hint={file.type} tokens={file.tokens} />
          ));
    case "skills":
      return usage.skills.length === 0
        ? empty
        : usage.skills.map((skill) => (
            <DetailRow
              key={`${skill.source ?? ""}:${skill.name}`}
              label={skill.name}
              hint={skill.source}
              tokens={skill.tokens}
            />
          ));
    case "agents":
      return usage.agents.length === 0
        ? empty
        : usage.agents.map((agent) => (
            <DetailRow
              key={`${agent.source ?? ""}:${agent.name}`}
              label={agent.name}
              hint={agent.source}
              tokens={agent.tokens}
            />
          ));
  }
}

const STATUS_LABEL: Record<ProviderMcpServer["status"], string> = {
  connected: "Connected",
  pending: "Connecting",
  "needs-auth": "Needs sign-in",
  failed: "Failed",
  disabled: "Disabled",
};

// Servers settle within seconds; back off so a stuck one does not keep the provider busy.
const PENDING_REFRESH_DELAYS_MS = [2_000, 5_000, 15_000, 30_000, 60_000];

/** `/mcp`: the servers the thread's live agent session loaded. */
export function ThreadMcpServersSheet({ route }: StaticScreenProps<SessionTarget>) {
  const target = route.params;
  const navigation = useNavigation();
  const query = useEnvironmentQuery(
    providerSessionEnvironment.mcpServers({
      environmentId: target.environmentId,
      input: { threadId: target.threadId },
    }),
  );
  const servers = query.data?.servers;
  const anyPending = servers?.some((server) => server.status === "pending") ?? false;
  const [pendingPolls, setPendingPolls] = useState(0);
  const { refresh, isPending } = query;
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- polls the provider while a server is still connecting
  useEffect(() => {
    if (!anyPending || isPending) return;
    const delay = PENDING_REFRESH_DELAYS_MS[pendingPolls];
    if (delay === undefined) return;
    const timer = setTimeout(() => {
      setPendingPolls((count) => count + 1);
      refresh();
    }, delay);
    return () => clearTimeout(timer);
  }, [anyPending, isPending, pendingPolls, refresh]);
  const refreshNow = () => {
    setPendingPolls(0);
    refresh();
  };
  return (
    <View collapsable={false} className="flex-1 bg-sheet">
      <SheetHeader title="MCP servers" onClose={() => navigation.goBack()} />
      <SheetBody>
        {query.error !== null ? (
          <Text className="text-danger-foreground">{query.error}</Text>
        ) : servers === undefined ? (
          <Muted>Loading…</Muted>
        ) : servers.length === 0 ? (
          <Muted>No MCP servers are configured.</Muted>
        ) : (
          servers.map((server) => (
            <McpServerRow
              key={server.name}
              server={server}
              target={target}
              onChanged={refreshNow}
            />
          ))
        )}
        <Button label="Refresh" size="compact" disabled={query.isPending} onPress={refreshNow} />
      </SheetBody>
    </View>
  );
}

function failureMessage(result: Parameters<typeof squashAtomCommandFailure>[0]): string {
  const error = squashAtomCommandFailure(result);
  return error instanceof Error && error.message.length > 0
    ? error.message
    : "The provider request failed.";
}

function McpServerRow(props: {
  readonly server: ProviderMcpServer;
  readonly target: SessionTarget;
  readonly onChanged: () => void;
}) {
  const { server, target } = props;
  const act = useAtomCommand(providerSessionEnvironment.mcpServerAction, { reportFailure: false });
  const submitCallback = useAtomCommand(providerSessionEnvironment.mcpOAuthCallback, {
    reportFailure: false,
  });
  const [busy, setBusy] = useState(false);
  const [awaitingCallback, setAwaitingCallback] = useState(false);
  const [showTools, setShowTools] = useState(false);
  const disabled = server.status === "disabled";

  const run = async (action: ProviderMcpServerAction) => {
    setBusy(true);
    const result = await act({
      environmentId: target.environmentId,
      input: { threadId: target.threadId, server: server.name, action },
    });
    setBusy(false);
    if (result._tag === "Failure") {
      Dialog.alert(`Could not update ${server.name}`, failureMessage(result));
      return;
    }
    if (action === "authenticate" && result.value.authUrl) {
      setAwaitingCallback(true);
      void Linking.openURL(result.value.authUrl).catch(() => undefined);
    }
    props.onChanged();
  };

  const pasteCallback = () =>
    Dialog.prompt({
      title: `Finish signing in to ${server.name}`,
      message:
        "After you approve access, the browser opens a page that fails to load. Copy that page's address and paste it here.",
      placeholder: "http://localhost:…/callback?code=…",
      initialValue: "",
      confirmText: "Submit",
      onConfirm: (value) => {
        const callbackUrl = value.trim();
        if (callbackUrl.length === 0) return;
        setBusy(true);
        void submitCallback({
          environmentId: target.environmentId,
          input: { threadId: target.threadId, server: server.name, callbackUrl },
        }).then((result) => {
          setBusy(false);
          if (result._tag === "Failure") {
            Dialog.alert(`Could not finish signing in to ${server.name}`, failureMessage(result));
            return;
          }
          setAwaitingCallback(false);
          props.onChanged();
        });
      },
    });

  return (
    <View className="gap-2 rounded-xl border border-border p-3">
      <View className="flex-row items-center gap-2">
        <View className="min-w-0 flex-1">
          <Text className="font-oni-medium" numberOfLines={1}>
            {server.name}
          </Text>
          <Muted>
            {[STATUS_LABEL[server.status], server.scope, server.transport]
              .filter(Boolean)
              .join(" · ")}
          </Muted>
        </View>
        <ThemedSwitch
          accessibilityLabel={disabled ? `Enable ${server.name}` : `Disable ${server.name}`}
          value={!disabled}
          disabled={busy}
          onValueChange={(enabled) => void run(enabled ? "enable" : "disable")}
        />
      </View>
      {server.error ? <Text className="text-sm text-danger-foreground">{server.error}</Text> : null}
      <View className="flex-row flex-wrap gap-2">
        {!disabled ? (
          <Button
            label="Reconnect"
            size="compact"
            disabled={busy}
            onPress={() => void run("reconnect")}
          />
        ) : null}
        {!disabled && mcpServerCanAuthenticate(server) ? (
          server.status === "connected" ? (
            <Button
              label="Sign out"
              size="compact"
              tone="text"
              disabled={busy}
              onPress={() => void run("clear-auth")}
            />
          ) : (
            <Button
              label="Sign in"
              size="compact"
              disabled={busy}
              onPress={() => void run("authenticate")}
            />
          )
        ) : null}
        {awaitingCallback ? (
          <Button
            label="Paste sign-in address"
            size="compact"
            tone="text"
            disabled={busy}
            onPress={pasteCallback}
          />
        ) : null}
      </View>
      {server.tools.length > 0 ? (
        <Pressable
          accessibilityRole="button"
          accessibilityState={{ expanded: showTools }}
          onPress={() => setShowTools((value) => !value)}
          className="active:opacity-70"
        >
          <Muted>
            {`${showTools ? "Hide" : "Show"} ${server.tools.length} ${server.tools.length === 1 ? "tool" : "tools"}`}
          </Muted>
        </Pressable>
      ) : null}
      {showTools
        ? server.tools.map((tool) => (
            <View key={tool.name}>
              <Text textRole="footnote" className="font-mono">
                {tool.name}
              </Text>
              {tool.description ? <Muted>{tool.description}</Muted> : null}
            </View>
          ))
        : null}
    </View>
  );
}
