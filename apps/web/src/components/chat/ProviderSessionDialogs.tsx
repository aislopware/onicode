import {
  contextCategoryAccents,
  contextCategoryDetail,
  type ContextCategoryAccent,
  contextUsedCategories,
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
import { type ReactNode, useEffect, useRef, useState } from "react";

import { resolveDiffPathForWorkspace } from "../../diffFileActions";
import { useOpenInPreferredEditor } from "../../editorPreferences";
import { contextChipColorVariable } from "../../lib/contextChipAccents";
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

const neutral = (percent: number) =>
  `color-mix(in oklab, var(--color-muted-foreground) ${percent}%, transparent)`;

// Each theme solves chip colors for contrast; the chroma floor keeps parts apart where a light
// solve washes them out.
function categoryColor(
  category: ProviderContextUsageCategory,
  accent: ContextCategoryAccent | null,
): string {
  if (accent !== null) {
    return `oklch(from var(${contextChipColorVariable(accent)}) l max(c, 0.12) h / 80%)`;
  }
  switch (category.kind) {
    case "free":
      return neutral(15);
    case "buffer":
      return neutral(35);
    case "deferred":
      return neutral(25);
    case "used":
      return neutral(60);
  }
}

/** What fills the used context, largest first, with the colors the breakdown uses. */
export function contextUsedParts(usage: ProviderContextUsage) {
  return contextUsedCategories(usage).map(({ category, accent }) => ({
    category,
    color: categoryColor(category, accent),
  }));
}

function percentOf(tokens: number, max: number): string {
  if (max <= 0) return "";
  const percent = (tokens / max) * 100;
  return percent < 0.1 ? "<0.1%" : `${percent.toFixed(1)}%`;
}

export function SessionError(props: { readonly error: string }) {
  return <p className="text-sm text-destructive-foreground">{props.error}</p>;
}

function useContextUsage(target: SessionTarget | null) {
  return useEnvironmentQuery(
    target === null
      ? null
      : providerSessionEnvironment.contextUsage({
          environmentId: target.environmentId,
          input: { threadId: target.threadId },
        }),
  );
}

/** The `/context` breakdown while `target` is set, asked again whenever the live total moves. */
export function useContextUsageBreakdown(target: SessionTarget | null, updatedAt: string) {
  const query = useContextUsage(target);
  const { refresh } = query;
  const enabled = target !== null;
  const seenUpdatedAt = useRef(updatedAt);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- asks the provider again when the live context total moves, so the rows match the Context row
  useEffect(() => {
    if (seenUpdatedAt.current === updatedAt) return;
    seenUpdatedAt.current = updatedAt;
    if (enabled) refresh();
  }, [enabled, updatedAt, refresh]);
  return query;
}

/**
 * The `/context` breakdown in the thread details card, largest first. Bars compare the parts of
 * the used context; free space and the compaction buffer stay with the Context row.
 */
export function ContextUsageDetails(props: {
  readonly usage: ProviderContextUsage;
  readonly target: SessionTarget;
}) {
  const { usage, target } = props;
  const [expanded, setExpanded] = useState<string | null>(null);
  const used = contextUsedParts(usage);
  const deferred = usage.categories.filter(
    (category) => category.kind === "deferred" && category.tokens > 0,
  );
  const largest = used[0]?.category.tokens ?? 0;
  const row = (category: ProviderContextUsageCategory, color: string | null) => {
    const key = category.id ?? category.name;
    return (
      <CompactCategoryRow
        key={key}
        category={category}
        bar={color !== null && largest > 0 ? { color, size: category.tokens / largest } : null}
        open={expanded === key}
        onToggle={() => setExpanded(expanded === key ? null : key)}
        renderDetail={(detail) => (
          <CategoryDetail detail={detail} usage={usage} target={target} compact />
        )}
      />
    );
  };
  return (
    <div className="flex flex-col text-xs">
      <ul className="flex flex-col">{used.map(({ category, color }) => row(category, color))}</ul>
      {deferred.length > 0 ? (
        <>
          <p className="px-2.5 pt-2 pb-0.5 text-2xs text-muted-foreground/70">Loaded on use</p>
          <ul className="flex flex-col">{deferred.map((category) => row(category, null))}</ul>
        </>
      ) : null}
    </div>
  );
}

function CompactCategoryRow(props: {
  readonly category: ProviderContextUsageCategory;
  /** Bar color and length from 0 to 1, or null for rows that take no room yet. */
  readonly bar: { readonly color: string; readonly size: number } | null;
  readonly open: boolean;
  readonly onToggle: () => void;
  readonly renderDetail: (
    detail: NonNullable<ReturnType<typeof contextCategoryDetail>>,
  ) => ReactNode;
}) {
  const { category, bar } = props;
  const detail = contextCategoryDetail(category);
  const name = category.name.replace(/\s*\(deferred\)$/i, "");
  const content = (
    <>
      <span
        className={cn(
          "flex min-w-0 items-center gap-1",
          bar === null ? "text-muted-foreground/70" : "text-muted-foreground",
        )}
      >
        <span className="truncate">{name}</span>
        {detail !== null ? (
          <ChevronRightIcon
            className={cn("size-3 shrink-0 opacity-60", props.open && "rotate-90")}
            aria-hidden
          />
        ) : null}
      </span>
      <span className="flex h-[3px]">
        {bar !== null ? (
          <span
            className="rounded-full"
            style={{ width: `max(${bar.size * 100}%, 3px)`, backgroundColor: bar.color }}
          />
        ) : null}
      </span>
      <span
        className={cn(
          "text-end tabular-nums",
          bar === null ? "text-muted-foreground/70" : "text-foreground/80",
        )}
      >
        {formatContextWindowTokens(category.tokens)}
      </span>
    </>
  );
  const rowClass =
    "grid h-6 w-full grid-cols-[minmax(0,1fr)_2.5rem_2.25rem] items-center gap-x-3 rounded-lg px-2.5 text-start";
  return (
    <li>
      {detail !== null ? (
        <button
          type="button"
          aria-expanded={props.open}
          className={cn(
            rowClass,
            "outline-none hover:bg-black/[0.035] focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring dark:hover:bg-white/[0.08]",
          )}
          onClick={props.onToggle}
        >
          {content}
        </button>
      ) : (
        <div className={rowClass}>{content}</div>
      )}
      {detail !== null && props.open ? (
        <div className="ms-2.5 mb-1 border-s border-border/65 ps-1">
          {props.renderDetail(detail)}
        </div>
      ) : null}
    </li>
  );
}

function ContextUsageContent(props: SessionTarget) {
  const query = useContextUsage(props);
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
  const colored = contextCategoryAccents(usage.categories)
    .filter(({ category }) => category.tokens > 0)
    .map(({ category, accent }) => ({ category, color: categoryColor(category, accent) }));
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
              style={{
                width: `${(category.tokens / Math.max(usage.maxTokens, 1)) * 100}%`,
                backgroundColor: color,
              }}
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
              <span
                className="size-2 shrink-0 rounded-full"
                style={{ backgroundColor: color }}
                aria-hidden
              />
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
  readonly compact?: boolean | undefined;
}) {
  const content = (
    <>
      <span className={cn("min-w-0 flex-1 truncate", props.compact && "text-muted-foreground")}>
        {props.label}
      </span>
      {props.hint ? (
        <span className={cn("truncate text-muted-foreground", props.compact && "opacity-70")}>
          {props.hint}
        </span>
      ) : null}
      <span className="tabular-nums text-muted-foreground">
        {formatContextWindowTokens(props.tokens)}
      </span>
    </>
  );
  const rowClass = props.compact
    ? "flex h-6 items-center gap-2 rounded-lg ps-2 pe-2.5 text-xs"
    : "flex items-center gap-2 rounded-md px-2 py-1 text-xs";
  return props.onClick ? (
    <button
      type="button"
      className={cn(
        rowClass,
        "w-full text-start focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
        props.compact ? "hover:bg-black/[0.035] dark:hover:bg-white/[0.08]" : "hover:bg-accent",
      )}
      onClick={props.onClick}
    >
      {content}
    </button>
  ) : (
    <div className={rowClass}>{content}</div>
  );
}

function CategoryDetail(props: {
  readonly detail: NonNullable<ReturnType<typeof contextCategoryDetail>>;
  readonly usage: ProviderContextUsage;
  readonly target: SessionTarget;
  /** The thread details card's rows. */
  readonly compact?: boolean;
}) {
  const { usage, target, compact } = props;
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
      className={compact ? "self-start" : "mt-1 self-start"}
      size="xs"
      variant={compact ? "ghost-muted" : "outline"}
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
                  compact={compact}
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
                  // The card is too narrow for a path; its user/project/local hint tells them apart.
                  label={compact ? (file.path.split(/[\\/]/).pop() ?? file.path) : file.path}
                  hint={file.type}
                  tokens={file.tokens}
                  compact={compact}
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
                  compact={compact}
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
                  compact={compact}
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
