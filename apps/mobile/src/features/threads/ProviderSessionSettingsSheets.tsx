import { PROVIDER_SESSION_INFO_VIEWS } from "@onicode/client-runtime/provider-session-views";
import type {
  ProviderPermissionBehavior,
  ProviderSessionInfo,
  ProviderSessionInfoOf,
} from "@onicode/contracts";
import { formatTokens } from "@onicode/shared/usageFormat";
import { useNavigation, type StaticScreenProps } from "@react-navigation/native";
import { useState } from "react";
import { View } from "react-native";

import { AppText as Text } from "../../components/ui/AppText";
import { Button } from "../../components/ui/Button";
import { ListRow } from "../../components/ui/ListRow";
import { ListSection } from "../../components/ui/ListSection";
import { Notice } from "../../components/ui/Notice";
import { SheetHeader } from "../../components/ui/SheetHeader";
import { cn } from "../../lib/cn";
import { providerSessionEnvironment } from "../../state/providerSession";
import { useEnvironmentQuery } from "../../state/query";
import { Muted, SheetBody, type SessionTarget } from "./ProviderSessionSheets";

export type ProviderSessionSettingsView = keyof typeof PROVIDER_SESSION_INFO_VIEWS;

const TITLES: Record<ProviderSessionSettingsView, string> = {
  status: "Status",
  memory: "Memory",
  agents: "Agents",
  skills: "Skills",
  hooks: "Hooks",
  permissions: "Permissions",
  plugin: "Plugins",
  "output-style": "Output style",
};

const BEHAVIORS: ReadonlyArray<{ value: ProviderPermissionBehavior; label: string }> = [
  { value: "allow", label: "Allow" },
  { value: "ask", label: "Ask" },
  { value: "deny", label: "Deny" },
];

type MemoryEntry = { readonly path: string; readonly label: string };

/** `/status`, `/memory`, `/agents`…: read-only views of the thread's live session; changes go through the agent. */
export function ThreadSessionSettingsSheet({
  route,
}: StaticScreenProps<SessionTarget & { readonly view: ProviderSessionSettingsView }>) {
  const { view, environmentId, threadId } = route.params;
  const target = { environmentId, threadId };
  const [viewing, setViewing] = useState<MemoryEntry | null>(null);
  return viewing === null ? (
    <SessionInfoSheet target={target} view={view} onOpenMemory={setViewing} />
  ) : (
    <MemoryFileViewer target={target} file={viewing} onDone={() => setViewing(null)} />
  );
}

function SessionInfoSheet(props: {
  readonly target: SessionTarget;
  readonly view: ProviderSessionSettingsView;
  readonly onOpenMemory: (file: MemoryEntry) => void;
}) {
  const { target } = props;
  const navigation = useNavigation();
  const query = useEnvironmentQuery(
    providerSessionEnvironment.info({
      environmentId: target.environmentId,
      input: { threadId: target.threadId, view: PROVIDER_SESSION_INFO_VIEWS[props.view] },
    }),
  );
  const info = query.data;
  return (
    <View collapsable={false} className="flex-1 bg-sheet">
      <SheetHeader title={TITLES[props.view]} onClose={() => navigation.goBack()} />
      <SheetBody>
        {query.error !== null ? (
          <Text className="text-danger-foreground">{query.error}</Text>
        ) : info === null ? (
          <Muted>Loading…</Muted>
        ) : (
          <SessionInfoContent info={info} onOpenMemory={props.onOpenMemory} />
        )}
        <Button label="Refresh" size="compact" disabled={query.isPending} onPress={query.refresh} />
      </SheetBody>
    </View>
  );
}

function SessionInfoContent(props: {
  readonly info: ProviderSessionInfo;
  readonly onOpenMemory: (file: MemoryEntry) => void;
}) {
  const { info } = props;
  switch (info._tag) {
    case "status":
      return <StatusInfo info={info} />;
    case "agents":
      return <AgentsInfo info={info} />;
    case "skills":
      return <SkillsInfo info={info} />;
    case "memory":
      return <MemoryInfo info={info} onOpen={props.onOpenMemory} />;
    case "output-styles":
      return <OutputStylesInfo info={info} />;
    case "hooks":
      return <HooksInfo info={info} />;
    case "permissions":
      return <PermissionsInfo info={info} />;
    case "plugins":
      return <PluginsInfo info={info} />;
  }
}

function Row(props: { readonly index: number; readonly children: React.ReactNode }) {
  return (
    <View className={cn("gap-1 px-4 py-3", props.index > 0 && "border-t border-border-subtle")}>
      {props.children}
    </View>
  );
}

function RowTitle(props: { readonly children: React.ReactNode; readonly mono?: boolean }) {
  return (
    <Text
      selectable
      className={cn("min-w-0 flex-1 font-oni-medium", props.mono && "font-mono text-sm")}
      numberOfLines={2}
    >
      {props.children}
    </Text>
  );
}

function StatusInfo(props: { readonly info: ProviderSessionInfoOf<"status"> }) {
  return props.info.sections.map((section) => (
    <ListSection key={section.title} title={section.title}>
      {section.rows.map((row, index) => (
        <Row key={row.label} index={index}>
          <View className="flex-row gap-3">
            <Muted className="max-w-[45%] shrink-0">{row.label}</Muted>
            <Text selectable textRole="footnote" className="min-w-0 flex-1 text-right">
              {row.value}
            </Text>
          </View>
        </Row>
      ))}
    </ListSection>
  ));
}

function AgentsInfo(props: { readonly info: ProviderSessionInfoOf<"agents"> }) {
  const { agents } = props.info;
  if (agents.length === 0) return <Muted>No subagents are configured.</Muted>;
  return (
    <ListSection>
      {agents.map((agent, index) => (
        <Row key={agent.name} index={index}>
          <View className="flex-row items-center gap-2">
            <RowTitle>{agent.name}</RowTitle>
            {agent.model ? <Muted>{agent.model}</Muted> : null}
          </View>
          {agent.description ? <Muted numberOfLines={2}>{agent.description}</Muted> : null}
        </Row>
      ))}
    </ListSection>
  );
}

function SkillsInfo(props: { readonly info: ProviderSessionInfoOf<"skills"> }) {
  const { skills } = props.info;
  if (skills.length === 0) return <Muted>No skills are installed.</Muted>;
  return (
    <ListSection>
      {skills.map((skill, index) => (
        <Row key={`${skill.source}:${skill.name}`} index={index}>
          <View className="flex-row items-center gap-2">
            <RowTitle>{skill.name}</RowTitle>
            {skill.enabled ? null : <Muted>Off</Muted>}
          </View>
          <Muted>
            {[
              skill.source,
              skill.tokens === undefined ? null : `${formatTokens(skill.tokens)} tokens`,
            ]
              .filter(Boolean)
              .join(" · ")}
          </Muted>
          {skill.description ? <Muted>{skill.description}</Muted> : null}
        </Row>
      ))}
    </ListSection>
  );
}

function MemoryInfo(props: {
  readonly info: ProviderSessionInfoOf<"memory">;
  readonly onOpen: (file: MemoryEntry) => void;
}) {
  const { files, memories, autoMemory } = props.info;
  return (
    <>
      {files.length === 0 ? (
        <Muted>No memory files are available.</Muted>
      ) : (
        <ListSection title="Memory files">
          {files.map((file, index) => (
            <View key={file.path} className={cn(index > 0 && "border-t border-border-subtle")}>
              <ListRow
                label={file.label}
                {...(file.description ? { description: file.description } : {})}
                meta={
                  <Text textRole="caption" className="font-mono text-foreground-muted">
                    {file.exists ? file.path : `${file.path} · Not created yet`}
                  </Text>
                }
                onPress={() => props.onOpen({ path: file.path, label: file.label })}
              />
            </View>
          ))}
        </ListSection>
      )}
      {memories.length > 0 || autoMemory !== undefined ? (
        <ListSection
          title="Auto memory"
          {...(autoMemory === undefined ? {} : { footer: `Auto memory is ${autoMemory}.` })}
        >
          {memories.length === 0 ? (
            <Row index={0}>
              <Muted>No memories saved yet.</Muted>
            </Row>
          ) : (
            memories.map((memory, index) => (
              <View key={memory.path} className={cn(index > 0 && "border-t border-border-subtle")}>
                <ListRow
                  label={memory.name}
                  {...(memory.description ? { description: memory.description } : {})}
                  onPress={() => props.onOpen({ path: memory.path, label: memory.name })}
                />
              </View>
            ))
          )}
        </ListSection>
      ) : null}
    </>
  );
}

function MemoryFileViewer(props: {
  readonly target: SessionTarget;
  readonly file: MemoryEntry;
  readonly onDone: () => void;
}) {
  const { target, file } = props;
  const query = useEnvironmentQuery(
    providerSessionEnvironment.memoryFile({
      environmentId: target.environmentId,
      input: { threadId: target.threadId, path: file.path },
    }),
  );
  const loaded = query.data;
  return (
    <View collapsable={false} className="flex-1 bg-sheet">
      <SheetHeader title={file.label} subtitle={file.path} onClose={props.onDone} />
      <SheetBody>
        {query.error !== null ? (
          <Text className="text-danger-foreground">{query.error}</Text>
        ) : loaded === null ? (
          <Muted>Loading…</Muted>
        ) : !loaded.exists ? (
          <Muted>Not created yet.</Muted>
        ) : loaded.content.trim().length === 0 ? (
          <Muted>This file is empty.</Muted>
        ) : (
          <Text selectable textRole="footnote" className="font-mono">
            {loaded.content}
          </Text>
        )}
      </SheetBody>
    </View>
  );
}

function OutputStylesInfo(props: { readonly info: ProviderSessionInfoOf<"output-styles"> }) {
  const { current, available } = props.info;
  return (
    <ListSection>
      {available.map((style, index) => (
        <Row key={style} index={index}>
          <View className="flex-row items-center gap-2">
            <RowTitle>{style}</RowTitle>
            {style === current ? <Muted>Current</Muted> : null}
          </View>
        </Row>
      ))}
    </ListSection>
  );
}

function HooksInfo(props: { readonly info: ProviderSessionInfoOf<"hooks"> }) {
  const { hooks, disabledByPolicy } = props.info;
  const events: Array<{ event: string; summary: string; hooks: typeof hooks }> = [];
  for (const hook of hooks) {
    const group = events.find((entry) => entry.event === hook.event);
    if (group === undefined) {
      events.push({ event: hook.event, summary: hook.eventSummary, hooks: [hook] });
    } else {
      group.hooks = [...group.hooks, hook];
    }
  }
  return (
    <>
      {disabledByPolicy ? <Notice message="Hooks are turned off by policy." /> : null}
      {events.length === 0 ? <Muted>No hooks are configured.</Muted> : null}
      {events.map((group) => (
        <ListSection
          key={group.event}
          title={group.event}
          {...(group.summary ? { footer: group.summary } : {})}
        >
          {group.hooks.map((hook, index) => (
            <Row key={`${hook.sourceLabel}:${hook.matcher}:${hook.commandText}`} index={index}>
              {hook.matcher ? (
                <Text selectable textRole="footnote" className="font-mono">
                  {hook.matcher}
                </Text>
              ) : null}
              <RowTitle>{hook.displayText}</RowTitle>
              {hook.commandText && hook.commandText !== hook.displayText ? (
                <Text
                  selectable
                  textRole="caption"
                  className="font-mono text-foreground-muted"
                  numberOfLines={3}
                >
                  {hook.commandText}
                </Text>
              ) : null}
              <Muted>{hook.sourceLabel}</Muted>
            </Row>
          ))}
        </ListSection>
      ))}
    </>
  );
}

function PermissionsInfo(props: { readonly info: ProviderSessionInfoOf<"permissions"> }) {
  const { rules, directories } = props.info;
  return (
    <>
      {BEHAVIORS.map(({ value, label }) => {
        const group = rules.filter((rule) => rule.behavior === value);
        return (
          <ListSection key={value} title={label}>
            {group.length === 0 ? (
              <Row index={0}>
                <Muted>No rules.</Muted>
              </Row>
            ) : (
              group.map((rule, index) => (
                <Row key={`${rule.rule}:${rule.source}`} index={index}>
                  <RowTitle mono>{rule.rule}</RowTitle>
                  {rule.description && rule.description !== rule.rule ? (
                    <Muted>{rule.description}</Muted>
                  ) : null}
                  <Muted>{rule.source}</Muted>
                </Row>
              ))
            )}
          </ListSection>
        );
      })}
      {directories.length > 0 ? (
        <ListSection title="Additional directories">
          {directories.map((directory, index) => (
            <Row key={directory.path} index={index}>
              <RowTitle mono>{directory.path}</RowTitle>
              <Muted>{directory.source}</Muted>
            </Row>
          ))}
        </ListSection>
      ) : null}
    </>
  );
}

function PluginsInfo(props: { readonly info: ProviderSessionInfoOf<"plugins"> }) {
  const { installed, marketplaces } = props.info;
  return (
    <>
      <ListSection title="Installed">
        {installed.length === 0 ? (
          <Row index={0}>
            <Muted>No plugins are installed.</Muted>
          </Row>
        ) : (
          installed.map((plugin, index) => (
            <Row key={`${plugin.id}:${plugin.scope}`} index={index}>
              <View className="flex-row items-center gap-2">
                <RowTitle>{plugin.id}</RowTitle>
                {plugin.enabled ? null : <Muted>Off</Muted>}
              </View>
              <Muted>{[plugin.version, plugin.scope].filter(Boolean).join(" · ")}</Muted>
            </Row>
          ))
        )}
      </ListSection>
      <ListSection title="Marketplaces">
        {marketplaces.length === 0 ? (
          <Row index={0}>
            <Muted>No marketplaces are added.</Muted>
          </Row>
        ) : (
          marketplaces.map((marketplace, index) => (
            <Row key={marketplace.name} index={index}>
              <RowTitle>{marketplace.name}</RowTitle>
              <Muted>{marketplace.source}</Muted>
            </Row>
          ))
        )}
      </ListSection>
    </>
  );
}
