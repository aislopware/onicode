import {
  contextCategoryDetail,
  contextUsageMcpTokensByServer,
  mcpServerCanAuthenticate,
} from "@onicode/client-runtime/provider-session-views";
import { squashAtomCommandFailure } from "@onicode/client-runtime/state/runtime";
import type {
  EditorId,
  EnvironmentId,
  ProviderContextUsage,
  ProviderContextUsageCategory,
  ProviderMcpServer,
  ProviderMcpServerAction,
  ScopedThreadRef,
  ThreadId,
} from "@onicode/contracts";
import { ChevronRightIcon } from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";

import { resolveDiffPathForWorkspace } from "../../diffFileActions";
import { useOpenInPreferredEditor } from "../../editorPreferences";
import { formatContextWindowTokens } from "../../lib/contextWindow";
import { cn } from "../../lib/utils";
import { readLocalApi } from "../../localApi";
import { useProviderSessionViewStore } from "../../providerSessionViewStore";
import { useRightPanelStore } from "../../rightPanelStore";
import { providerSessionEnvironment } from "../../state/providerSession";
import { useEnvironmentQuery } from "../../state/query";
import { useAtomCommand } from "../../state/use-atom-command";
import { Badge } from "../ui/badge";
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
import { Input } from "../ui/input";
import { Switch } from "../ui/switch";
import { toastManager } from "../ui/toast";

export interface SessionTarget {
  readonly environmentId: EnvironmentId;
  readonly threadId: ThreadId;
  readonly threadRef: ScopedThreadRef;
  readonly threadKey: string;
  readonly cwd: string | null;
  readonly availableEditors: ReadonlyArray<EditorId>;
}

/** The thread's `/context` and `/mcp` views. They load only while open. */
export function ProviderSessionDialogs(props: SessionTarget) {
  const open = useProviderSessionViewStore((state) =>
    state.open?.threadKey === props.threadKey ? state.open.view : null,
  );
  const close = useProviderSessionViewStore((state) => state.close);
  const onOpenChange = (next: boolean) => {
    if (!next) close();
  };
  return (
    <>
      <Dialog open={open === "context"} onOpenChange={onOpenChange}>
        <DialogPopup className="max-w-xl">
          {open === "context" ? <ContextUsageContent {...props} /> : null}
        </DialogPopup>
      </Dialog>
      <Dialog open={open === "mcp"} onOpenChange={onOpenChange}>
        <DialogPopup className="max-w-xl">
          {open === "mcp" ? <McpServersContent {...props} /> : null}
        </DialogPopup>
      </Dialog>
    </>
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

function categoryColor(category: ProviderContextUsageCategory, usedIndex: number): string {
  switch (category.kind) {
    case "free":
      return "bg-muted-foreground/15";
    case "buffer":
      return "bg-muted-foreground/35";
    case "deferred":
      return "bg-muted-foreground/25";
    case "used":
      return USED_COLORS[usedIndex % USED_COLORS.length]!;
  }
}

function percentOf(tokens: number, max: number): string {
  if (max <= 0) return "";
  const percent = (tokens / max) * 100;
  return percent < 0.1 ? "<0.1%" : `${percent.toFixed(1)}%`;
}

export function SessionError(props: { readonly error: string }) {
  return <p className="text-sm text-destructive-foreground">{props.error}</p>;
}

function ContextUsageContent(props: SessionTarget) {
  const query = useEnvironmentQuery(
    providerSessionEnvironment.contextUsage({
      environmentId: props.environmentId,
      input: { threadId: props.threadId },
    }),
  );
  const usage = query.data;
  return (
    <>
      <DialogHeader>
        <DialogTitle>Context window</DialogTitle>
        <DialogDescription>
          {usage
            ? `${usage.model} · ${formatContextWindowTokens(usage.totalTokens)} of ${formatContextWindowTokens(usage.maxTokens)} tokens (${Math.round(usage.percentage)}%)`
            : "What the agent's next request carries."}
        </DialogDescription>
      </DialogHeader>
      <DialogPanel>
        {query.error !== null ? (
          <SessionError error={query.error} />
        ) : usage === null ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : (
          <ContextBreakdown usage={usage} target={props} />
        )}
      </DialogPanel>
      <DialogFooter variant="bare">
        <Button size="sm" variant="outline" disabled={query.isPending} onClick={query.refresh}>
          Refresh
        </Button>
      </DialogFooter>
    </>
  );
}

function ContextBreakdown(props: {
  readonly usage: ProviderContextUsage;
  readonly target: SessionTarget;
}) {
  const { usage } = props;
  const [expanded, setExpanded] = useState<string | null>(null);
  const categories = usage.categories.filter((category) => category.tokens > 0);
  const colored = categories.map((category, index) => ({
    category,
    color: categoryColor(
      category,
      categories.slice(0, index).filter((previous) => previous.kind === "used").length,
    ),
  }));
  return (
    <div className="flex flex-col gap-3">
      <div
        className="flex h-2 w-full overflow-hidden rounded-full bg-muted"
        role="img"
        aria-label={`${Math.round(usage.percentage)}% of the context window used`}
      >
        {/* Deferred tools load on demand, so they take no room until used. */}
        {colored
          .filter(({ category }) => category.kind !== "deferred")
          .map(({ category, color }) => (
            <span
              key={category.id ?? category.name}
              className={color}
              style={{ width: `${(category.tokens / Math.max(usage.maxTokens, 1)) * 100}%` }}
            />
          ))}
      </div>
      <ul className="flex flex-col text-sm">
        {colored.map(({ category, color }) => {
          const key = category.id ?? category.name;
          const detail = contextCategoryDetail(category);
          const isOpen = expanded === key;
          const row = (
            <>
              <span className={cn("size-2 shrink-0 rounded-full", color)} aria-hidden />
              <span className="min-w-0 flex-1 truncate">{category.name}</span>
              <span className="tabular-nums text-muted-foreground">
                {formatContextWindowTokens(category.tokens)}
              </span>
              <span className="w-12 text-end tabular-nums text-muted-foreground">
                {category.kind === "deferred"
                  ? "on use"
                  : percentOf(category.tokens, usage.maxTokens)}
              </span>
            </>
          );
          return (
            <li key={key}>
              {detail === null ? (
                <div className="flex items-center gap-2 px-2 py-1.5">
                  <span className="size-3.5 shrink-0" aria-hidden />
                  {row}
                </div>
              ) : (
                <button
                  type="button"
                  aria-expanded={isOpen}
                  className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-start hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
                  onClick={() => setExpanded(isOpen ? null : key)}
                >
                  <ChevronRightIcon
                    className={cn("size-3.5 shrink-0 text-muted-foreground", isOpen && "rotate-90")}
                    aria-hidden
                  />
                  {row}
                </button>
              )}
              {isOpen && detail !== null ? (
                <div className="mb-1 ms-7 border-s ps-3">
                  <CategoryDetail detail={detail} usage={usage} target={props.target} />
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>
      <p className="text-xs text-muted-foreground">
        {usage.isAutoCompactEnabled
          ? usage.autoCompactThreshold !== undefined
            ? `Auto-compacts at ${formatContextWindowTokens(usage.autoCompactThreshold)} tokens.`
            : "Auto-compact is on."
          : "Auto-compact is off."}
      </p>
    </div>
  );
}

function DetailRow(props: {
  readonly label: ReactNode;
  readonly hint?: string | undefined;
  readonly tokens: number;
  readonly onClick?: (() => void) | undefined;
}) {
  const content = (
    <>
      <span className="min-w-0 flex-1 truncate">{props.label}</span>
      {props.hint ? <span className="truncate text-muted-foreground">{props.hint}</span> : null}
      <span className="tabular-nums text-muted-foreground">
        {formatContextWindowTokens(props.tokens)}
      </span>
    </>
  );
  return props.onClick ? (
    <button
      type="button"
      className="flex w-full items-center gap-2 rounded-md px-2 py-1 text-start text-xs hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
      onClick={props.onClick}
    >
      {content}
    </button>
  ) : (
    <div className="flex items-center gap-2 px-2 py-1 text-xs">{content}</div>
  );
}

function CategoryDetail(props: {
  readonly detail: NonNullable<ReturnType<typeof contextCategoryDetail>>;
  readonly usage: ProviderContextUsage;
  readonly target: SessionTarget;
}) {
  const { usage, target } = props;
  const show = useProviderSessionViewStore((state) => state.show);
  const openInPreferredEditor = useOpenInPreferredEditor(
    target.environmentId,
    target.availableEditors,
  );
  const openMemoryFile = (path: string) => {
    const workspacePath = resolveDiffPathForWorkspace({
      filePath: path,
      workspaceRoot: target.cwd ?? undefined,
      repositoryRoot: undefined,
    });
    if (workspacePath !== null) {
      useRightPanelStore.getState().openFile(target.threadRef, workspacePath);
      useProviderSessionViewStore.getState().close();
      return;
    }
    void openInPreferredEditor(path).then((result) => {
      if (result._tag === "Failure") {
        toastManager.add({ type: "error", title: "Could not open the file", description: path });
      }
    });
  };
  const empty = <p className="px-2 py-1 text-xs text-muted-foreground">Nothing listed.</p>;
  const manage = (view: "mcp" | "memory" | "skills" | "agents", label: string) => (
    <Button
      className="mt-1 self-start"
      size="xs"
      variant="outline"
      onClick={() => show(target.threadKey, view)}
    >
      {label}
    </Button>
  );
  switch (props.detail) {
    case "mcp": {
      const servers = contextUsageMcpTokensByServer(usage);
      return (
        <div className="flex flex-col">
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
          {manage("mcp", "Manage MCP servers")}
        </div>
      );
    }
    case "memory":
      return (
        <div className="flex flex-col">
          {usage.memoryFiles.length === 0
            ? empty
            : usage.memoryFiles.map((file) => (
                <DetailRow
                  key={file.path}
                  label={file.path}
                  hint={file.type}
                  tokens={file.tokens}
                  onClick={() => openMemoryFile(file.path)}
                />
              ))}
          {manage("memory", "Show memory")}
        </div>
      );
    case "skills":
      return (
        <div className="flex flex-col">
          {usage.skills.length === 0
            ? empty
            : usage.skills.map((skill) => (
                <DetailRow
                  key={`${skill.source ?? ""}:${skill.name}`}
                  label={skill.name}
                  hint={skill.source}
                  tokens={skill.tokens}
                />
              ))}
          {manage("skills", "Show skills")}
        </div>
      );
    case "agents":
      return (
        <div className="flex flex-col">
          {usage.agents.length === 0
            ? empty
            : usage.agents.map((agent) => (
                <DetailRow
                  key={`${agent.source ?? ""}:${agent.name}`}
                  label={agent.name}
                  hint={agent.source}
                  tokens={agent.tokens}
                />
              ))}
          {manage("agents", "Show agents")}
        </div>
      );
  }
}

const STATUS_BADGE: Record<
  ProviderMcpServer["status"],
  {
    readonly label: string;
    readonly variant: "success" | "info" | "warning" | "error" | "secondary";
  }
> = {
  connected: { label: "Connected", variant: "success" },
  pending: { label: "Connecting", variant: "info" },
  "needs-auth": { label: "Needs sign-in", variant: "warning" },
  failed: { label: "Failed", variant: "error" },
  disabled: { label: "Disabled", variant: "secondary" },
};

// Servers settle within seconds; back off so a stuck one does not keep the provider busy.
const PENDING_REFRESH_DELAYS_MS = [2_000, 5_000, 15_000, 30_000, 60_000];

function McpServersContent(props: SessionTarget) {
  const query = useEnvironmentQuery(
    providerSessionEnvironment.mcpServers({
      environmentId: props.environmentId,
      input: { threadId: props.threadId },
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
    <>
      <DialogHeader>
        <DialogTitle>MCP servers</DialogTitle>
        <DialogDescription>
          Servers this thread's agent loaded. Turning one off and signing out are saved in Claude
          Code's settings, so they also apply outside Onicode.
        </DialogDescription>
      </DialogHeader>
      <DialogPanel>
        {query.error !== null ? (
          <SessionError error={query.error} />
        ) : servers === undefined ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : servers.length === 0 ? (
          <p className="text-sm text-muted-foreground">No MCP servers are configured.</p>
        ) : (
          <ul className="flex flex-col gap-2">
            {servers.map((server) => (
              <McpServerRow
                key={server.name}
                server={server}
                target={props}
                onChanged={refreshNow}
              />
            ))}
          </ul>
        )}
      </DialogPanel>
      <DialogFooter variant="bare">
        <Button size="sm" variant="outline" disabled={query.isPending} onClick={refreshNow}>
          Refresh
        </Button>
      </DialogFooter>
    </>
  );
}

function McpServerRow(props: {
  readonly server: ProviderMcpServer;
  readonly target: SessionTarget;
  readonly onChanged: () => void;
}) {
  const { server, target } = props;
  const act = useAtomCommand(providerSessionEnvironment.mcpServerAction, {
    reportFailure: false,
  });
  const submitCallback = useAtomCommand(providerSessionEnvironment.mcpOAuthCallback, {
    reportFailure: false,
  });
  const [busy, setBusy] = useState<ProviderMcpServerAction | "callback" | null>(null);
  const [showTools, setShowTools] = useState(false);
  const [awaitingCallback, setAwaitingCallback] = useState(false);
  const [callbackUrl, setCallbackUrl] = useState("");
  const badge = STATUS_BADGE[server.status];
  const disabled = server.status === "disabled";

  const run = async (action: ProviderMcpServerAction) => {
    setBusy(action);
    const result = await act({
      environmentId: target.environmentId,
      input: { threadId: target.threadId, server: server.name, action },
    });
    setBusy(null);
    if (result._tag === "Failure") {
      toastManager.add({
        type: "error",
        title: `Could not update ${server.name}`,
        description: failureMessage(result),
      });
      return;
    }
    if (action === "authenticate" && result.value.authUrl) {
      setAwaitingCallback(true);
      void readLocalApi()
        ?.shell.openExternal(result.value.authUrl)
        .catch(() => undefined);
    }
    props.onChanged();
  };

  const finishSignIn = async () => {
    const url = callbackUrl.trim();
    if (url.length === 0) return;
    setBusy("callback");
    const result = await submitCallback({
      environmentId: target.environmentId,
      input: { threadId: target.threadId, server: server.name, callbackUrl: url },
    });
    setBusy(null);
    if (result._tag === "Failure") {
      toastManager.add({
        type: "error",
        title: `Could not finish signing in to ${server.name}`,
        description: failureMessage(result),
      });
      return;
    }
    setAwaitingCallback(false);
    setCallbackUrl("");
    props.onChanged();
  };

  return (
    <li className="flex flex-col gap-2 rounded-lg border p-3">
      <div className="flex items-center gap-2">
        <span className="min-w-0 truncate font-medium text-sm">{server.name}</span>
        <Badge size="sm" variant={badge.variant}>
          {badge.label}
        </Badge>
        <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
          {[server.scope, server.transport].filter(Boolean).join(" · ")}
        </span>
        <Switch
          size="sm"
          aria-label={disabled ? `Enable ${server.name}` : `Disable ${server.name}`}
          checked={!disabled}
          disabled={busy !== null}
          onCheckedChange={(checked) => void run(checked ? "enable" : "disable")}
        />
      </div>
      {server.error ? (
        <p className="wrap-anywhere text-xs text-destructive-foreground">{server.error}</p>
      ) : null}
      <div className="flex flex-wrap items-center gap-1.5">
        {server.tools.length > 0 ? (
          <Button
            size="xs"
            variant="ghost"
            aria-expanded={showTools}
            onClick={() => setShowTools((value) => !value)}
          >
            <ChevronRightIcon className={cn(showTools && "rotate-90")} />
            {server.tools.length} {server.tools.length === 1 ? "tool" : "tools"}
          </Button>
        ) : null}
        {!disabled ? (
          <Button
            size="xs"
            variant="outline"
            disabled={busy !== null}
            onClick={() => void run("reconnect")}
          >
            Reconnect
          </Button>
        ) : null}
        {!disabled && mcpServerCanAuthenticate(server) ? (
          server.status === "connected" ? (
            <Button
              size="xs"
              variant="ghost"
              disabled={busy !== null}
              onClick={() => void run("clear-auth")}
            >
              Sign out
            </Button>
          ) : (
            <Button
              size="xs"
              variant="outline"
              disabled={busy !== null}
              onClick={() => void run("authenticate")}
            >
              Sign in
            </Button>
          )
        ) : null}
      </div>
      {awaitingCallback ? (
        <form
          className="flex flex-col gap-1.5"
          onSubmit={(event) => {
            event.preventDefault();
            void finishSignIn();
          }}
        >
          <p className="text-xs text-muted-foreground">
            Finish signing in in your browser. If it ends on a page that fails to load, paste that
            page's address here.
          </p>
          <div className="flex gap-1.5">
            <Input
              size="sm"
              placeholder="http://localhost:…/callback?code=…"
              value={callbackUrl}
              onChange={(event) => setCallbackUrl(event.target.value)}
              aria-label={`Sign-in address for ${server.name}`}
            />
            <Button
              size="sm"
              type="submit"
              disabled={busy !== null || callbackUrl.trim().length === 0}
            >
              Submit
            </Button>
          </div>
        </form>
      ) : null}
      {showTools ? (
        <ul className="flex flex-col gap-1 border-s ps-3">
          {server.tools.map((tool) => (
            <li key={tool.name} className="text-xs">
              <span className="font-mono">{tool.name}</span>
              {tool.description ? (
                <span className="line-clamp-2 text-muted-foreground">{tool.description}</span>
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}
    </li>
  );
}

function failureMessage(result: Parameters<typeof squashAtomCommandFailure>[0]): string {
  const error = squashAtomCommandFailure(result);
  return error instanceof Error && error.message.length > 0
    ? error.message
    : "The provider request failed.";
}
