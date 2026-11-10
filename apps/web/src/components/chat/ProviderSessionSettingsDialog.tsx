import {
  isProviderSessionSettingsView,
  PROVIDER_SESSION_INFO_VIEWS,
  type ProviderSessionSettingsView,
} from "@onicode/client-runtime/provider-session-views";
import type {
  ProviderPermissionBehavior,
  ProviderSessionInfo,
  ProviderSessionInfoOf,
} from "@onicode/contracts";
import { formatTokens } from "@onicode/shared/usageFormat";
import { ArrowLeftIcon, ChevronRightIcon } from "lucide-react";
import { useState } from "react";

import { useProviderSessionViewStore } from "../../providerSessionViewStore";
import { providerSessionEnvironment } from "../../state/providerSession";
import { useEnvironmentQuery } from "../../state/query";
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
import { Toggle, ToggleGroup } from "../ui/toggle-group";
import { SessionError, type SessionTarget } from "./ProviderSessionDialogs";

const TITLES: Record<ProviderSessionSettingsView, { title: string; description: string }> = {
  status: { title: "Status", description: "The agent process behind this thread." },
  memory: {
    title: "Memory",
    description: "Instruction files the agent reads at the start of every session.",
  },
  agents: { title: "Agents", description: "Subagents this thread's agent can hand work to." },
  skills: { title: "Skills", description: "Skills the agent can load when a task calls for one." },
  hooks: { title: "Hooks", description: "Commands Claude Code runs on session events." },
  permissions: {
    title: "Permissions",
    description: "Rules that let tools run, ask first, or never run.",
  },
  plugin: {
    title: "Plugins",
    description: "Plugins and marketplaces in your Claude Code settings.",
  },
  "output-style": { title: "Output style", description: "How the agent writes its replies." },
};

/** Claude Code's own settings, read-only: `/status`, `/memory`, `/plugin`… They load only while open. */
export function ProviderSessionSettingsDialog(props: SessionTarget) {
  const view = useProviderSessionViewStore((state) =>
    state.open?.threadKey === props.threadKey && isProviderSessionSettingsView(state.open.view)
      ? state.open.view
      : null,
  );
  const close = useProviderSessionViewStore((state) => state.close);
  return (
    <Dialog
      open={view !== null}
      onOpenChange={(next) => {
        if (!next) close();
      }}
    >
      <DialogPopup className="max-w-2xl">
        {view !== null ? <SettingsContent key={view} view={view} target={props} /> : null}
      </DialogPopup>
    </Dialog>
  );
}

function SettingsContent(props: {
  readonly view: ProviderSessionSettingsView;
  readonly target: SessionTarget;
}) {
  const { view, target } = props;
  const query = useEnvironmentQuery(
    providerSessionEnvironment.info({
      environmentId: target.environmentId,
      input: { threadId: target.threadId, view: PROVIDER_SESSION_INFO_VIEWS[view] },
    }),
  );
  const [openMemory, setOpenMemory] = useState<string | null>(null);
  const { title, description } = TITLES[view];

  if (openMemory !== null) {
    return <MemoryFileView target={target} path={openMemory} onBack={() => setOpenMemory(null)} />;
  }

  return (
    <>
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>{description}</DialogDescription>
      </DialogHeader>
      <DialogPanel>
        {query.error !== null ? (
          <SessionError error={query.error} />
        ) : query.data === null ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : (
          <SettingsBody info={query.data} onOpenMemory={setOpenMemory} />
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

function SettingsBody(props: {
  readonly info: ProviderSessionInfo;
  readonly onOpenMemory: (path: string) => void;
}) {
  const { info } = props;
  switch (info._tag) {
    case "status":
      return <StatusView info={info} />;
    case "agents":
      return <AgentsView info={info} />;
    case "skills":
      return <SkillsView info={info} />;
    case "memory":
      return <MemoryView info={info} onOpen={props.onOpenMemory} />;
    case "output-styles":
      return <OutputStylesView info={info} />;
    case "hooks":
      return <HooksView info={info} />;
    case "permissions":
      return <PermissionsView info={info} />;
    case "plugins":
      return <PluginsView info={info} />;
  }
}

function Empty(props: { readonly children: string }) {
  return <p className="text-sm text-muted-foreground">{props.children}</p>;
}

function StatusView(props: { readonly info: ProviderSessionInfoOf<"status"> }) {
  return (
    <div className="flex flex-col gap-4">
      {props.info.sections.map((section) => (
        <section key={section.title} className="flex flex-col gap-1.5">
          <h3 className="font-medium text-sm">{section.title}</h3>
          <dl className="grid grid-cols-[minmax(7rem,auto)_1fr] gap-x-4 gap-y-1 text-sm">
            {section.rows.map((row) => (
              <div key={row.label} className="contents">
                <dt className="text-muted-foreground">{row.label}</dt>
                <dd className="wrap-anywhere select-text">{row.value}</dd>
              </div>
            ))}
          </dl>
        </section>
      ))}
    </div>
  );
}

function AgentsView(props: { readonly info: ProviderSessionInfoOf<"agents"> }) {
  if (props.info.agents.length === 0) return <Empty>No agents are available.</Empty>;
  return (
    <ul className="flex flex-col gap-2">
      {props.info.agents.map((agent) => (
        <li key={agent.name} className="flex flex-col gap-0.5 rounded-lg border p-3">
          <div className="flex items-center gap-2">
            <span className="font-medium text-sm">{agent.name}</span>
            {agent.model ? (
              <Badge size="sm" variant="outline">
                {agent.model}
              </Badge>
            ) : null}
          </div>
          <p className="line-clamp-2 text-xs text-muted-foreground">{agent.description}</p>
        </li>
      ))}
    </ul>
  );
}

function OffBadge() {
  return (
    <Badge size="sm" variant="outline">
      Off
    </Badge>
  );
}

function SkillsView(props: { readonly info: ProviderSessionInfoOf<"skills"> }) {
  if (props.info.skills.length === 0) return <Empty>No skills are available.</Empty>;
  return (
    <ul className="flex flex-col divide-y">
      {props.info.skills.map((skill) => (
        <li key={skill.name} className="flex flex-col gap-0.5 py-2">
          <div className="flex items-center gap-2">
            <span className="truncate font-medium text-sm">{skill.name}</span>
            {skill.enabled ? null : <OffBadge />}
            <span className="shrink-0 text-xs text-muted-foreground">
              {[
                skill.source,
                skill.tokens === undefined ? null : `${formatTokens(skill.tokens)} tokens`,
              ]
                .filter(Boolean)
                .join(" · ")}
            </span>
          </div>
          <p className="line-clamp-2 text-xs text-muted-foreground">{skill.description}</p>
        </li>
      ))}
    </ul>
  );
}

function MemoryView(props: {
  readonly info: ProviderSessionInfoOf<"memory">;
  readonly onOpen: (path: string) => void;
}) {
  const { info } = props;
  const row = (key: string, label: string, detail: string, path: string, missing: boolean) => (
    <li key={key}>
      <button
        type="button"
        className="flex w-full items-center gap-3 rounded-lg px-2 py-2 text-start hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
        onClick={() => props.onOpen(path)}
      >
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="truncate font-medium text-sm">{label}</span>
          <span className="truncate text-xs text-muted-foreground">
            {missing ? `Not created yet · ${detail}` : detail}
          </span>
        </span>
        <ChevronRightIcon className="size-4 shrink-0 text-muted-foreground" />
      </button>
    </li>
  );
  return (
    <div className="flex flex-col gap-4">
      <ul className="flex flex-col">
        {info.files.map((file) =>
          row(file.path, file.label, file.description || file.path, file.path, !file.exists),
        )}
      </ul>
      {info.memories.length > 0 || info.autoMemory !== undefined ? (
        <section className="flex flex-col gap-1">
          <h3 className="px-2 font-medium text-sm">
            Auto memory{info.autoMemory ? ` · ${info.autoMemory}` : ""}
          </h3>
          <ul className="flex flex-col">
            {info.memories.map((memory) =>
              row(memory.path, memory.name, memory.description, memory.path, false),
            )}
          </ul>
        </section>
      ) : null}
    </div>
  );
}

function MemoryFileView(props: {
  readonly target: SessionTarget;
  readonly path: string;
  readonly onBack: () => void;
}) {
  const query = useEnvironmentQuery(
    providerSessionEnvironment.memoryFile({
      environmentId: props.target.environmentId,
      input: { threadId: props.target.threadId, path: props.path },
    }),
  );
  return (
    <>
      <DialogHeader>
        <div className="flex items-center gap-2">
          <Button size="icon-xs" variant="ghost" aria-label="Back to memory" onClick={props.onBack}>
            <ArrowLeftIcon />
          </Button>
          <DialogTitle>Memory file</DialogTitle>
        </div>
        <DialogDescription>
          <span className="wrap-anywhere font-mono">{props.path}</span>
        </DialogDescription>
      </DialogHeader>
      <DialogPanel>
        {query.error !== null ? (
          <SessionError error={query.error} />
        ) : query.data === null ? (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ) : !query.data.exists ? (
          <Empty>Not created yet.</Empty>
        ) : query.data.content.trim().length === 0 ? (
          <Empty>This file is empty.</Empty>
        ) : (
          <pre className="whitespace-pre-wrap wrap-anywhere select-text font-mono text-xs">
            {query.data.content}
          </pre>
        )}
      </DialogPanel>
    </>
  );
}

function OutputStylesView(props: { readonly info: ProviderSessionInfoOf<"output-styles"> }) {
  return (
    <ul className="flex flex-col divide-y">
      {props.info.available.map((style) => (
        <li key={style} className="flex items-center gap-2 py-2 text-sm">
          <span className="flex-1">{style}</span>
          {style === props.info.current ? (
            <Badge size="sm" variant="secondary">
              Current
            </Badge>
          ) : null}
        </li>
      ))}
    </ul>
  );
}

function HooksView(props: { readonly info: ProviderSessionInfoOf<"hooks"> }) {
  const { info } = props;
  const events = [...new Set(info.hooks.map((hook) => hook.event))];
  return (
    <div className="flex flex-col gap-4">
      {info.disabledByPolicy ? (
        <p className="text-sm text-warning-foreground">Hooks are turned off by policy.</p>
      ) : null}
      {events.length === 0 ? <Empty>No hooks are configured.</Empty> : null}
      {events.map((event) => {
        const hooks = info.hooks.filter((hook) => hook.event === event);
        return (
          <section key={event} className="flex flex-col gap-1.5">
            <h3 className="text-sm">
              <span className="font-medium">{event}</span>
              <span className="text-muted-foreground"> · {hooks[0]?.eventSummary}</span>
            </h3>
            <ul className="flex flex-col gap-2">
              {hooks.map((hook) => (
                <li
                  key={`${hook.sourceLabel}:${hook.matcher}:${hook.commandText}`}
                  className="flex flex-col gap-1 rounded-lg border p-3"
                >
                  <div className="flex flex-wrap items-center gap-2 text-sm">
                    {hook.matcher ? (
                      <Badge size="sm" variant="outline">
                        {hook.matcher}
                      </Badge>
                    ) : null}
                    <span className="min-w-0 flex-1 truncate">{hook.displayText}</span>
                    <span className="text-xs text-muted-foreground">{hook.sourceLabel}</span>
                  </div>
                  <pre className="line-clamp-3 whitespace-pre-wrap wrap-anywhere font-mono text-xs text-muted-foreground">
                    {hook.contentLabel}: {hook.commandText}
                  </pre>
                </li>
              ))}
            </ul>
          </section>
        );
      })}
    </div>
  );
}

const BEHAVIORS: ReadonlyArray<{ value: ProviderPermissionBehavior; label: string }> = [
  { value: "allow", label: "Allow" },
  { value: "ask", label: "Ask" },
  { value: "deny", label: "Deny" },
];

function PermissionsView(props: { readonly info: ProviderSessionInfoOf<"permissions"> }) {
  const { info } = props;
  const [behavior, setBehavior] = useState<ProviderPermissionBehavior>("allow");
  const rules = info.rules.filter((entry) => entry.behavior === behavior);
  return (
    <div className="flex flex-col gap-3">
      <ToggleGroup
        aria-label="Rule type"
        value={[behavior]}
        onValueChange={(next) => {
          const value = BEHAVIORS.find((entry) => entry.value === next[0]);
          if (value) setBehavior(value.value);
        }}
      >
        {BEHAVIORS.map((entry) => (
          <Toggle key={entry.value} value={entry.value}>
            {entry.label} ({info.rules.filter((r) => r.behavior === entry.value).length})
          </Toggle>
        ))}
      </ToggleGroup>
      {rules.length === 0 ? (
        <Empty>No rules of this type.</Empty>
      ) : (
        <ul className="flex flex-col divide-y">
          {rules.map((entry) => (
            <li key={`${entry.source}:${entry.rule}`} className="flex flex-col gap-0.5 py-2">
              <span className="wrap-anywhere font-mono text-sm">{entry.rule}</span>
              <span className="text-xs text-muted-foreground">
                {entry.description} · {entry.source}
              </span>
            </li>
          ))}
        </ul>
      )}
      {info.directories.length > 0 ? (
        <section className="flex flex-col gap-1">
          <h3 className="font-medium text-sm">Additional directories</h3>
          <ul className="flex flex-col gap-0.5 text-xs">
            {info.directories.map((directory) => (
              <li key={directory.path} className="wrap-anywhere">
                <span className="font-mono">{directory.path}</span>
                <span className="text-muted-foreground"> · {directory.source}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </div>
  );
}

function PluginsView(props: { readonly info: ProviderSessionInfoOf<"plugins"> }) {
  const { info } = props;
  return (
    <div className="flex flex-col gap-4">
      <section className="flex flex-col gap-1">
        <h3 className="font-medium text-sm">Installed</h3>
        {info.installed.length === 0 ? (
          <Empty>No plugins are installed.</Empty>
        ) : (
          <ul className="flex flex-col divide-y">
            {info.installed.map((plugin) => (
              <li key={`${plugin.scope}:${plugin.id}`} className="flex items-center gap-2 py-2">
                <span className="min-w-0 flex-1 truncate font-medium text-sm">{plugin.id}</span>
                {plugin.enabled ? null : <OffBadge />}
                <span className="shrink-0 text-xs text-muted-foreground">
                  {[plugin.version, plugin.scope].filter(Boolean).join(" · ")}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>
      <section className="flex flex-col gap-1">
        <h3 className="font-medium text-sm">Marketplaces</h3>
        {info.marketplaces.length === 0 ? (
          <Empty>No marketplaces are added.</Empty>
        ) : (
          <ul className="flex flex-col divide-y">
            {info.marketplaces.map((entry) => (
              <li key={entry.name} className="flex flex-col gap-0.5 py-2">
                <span className="font-medium text-sm">{entry.name}</span>
                <span className="wrap-anywhere text-xs text-muted-foreground">{entry.source}</span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
