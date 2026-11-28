import { useAtomValue } from "@effect/atom-react";
import { scopeThreadRef } from "@onicode/client-runtime/environment";
import { isAtomCommandInterrupted } from "@onicode/client-runtime/state/runtime";
import {
  type EnvironmentId,
  type HostPressure,
  type HostPressureAgent,
  type HostPressureKind,
  type RunId,
  hostPressureRanking,
  severeHostPressureKinds,
} from "@onicode/contracts";
import { useGT } from "gt-react-native";
import { useState } from "react";
import { Pressable, View } from "react-native";

import { AppText } from "../../components/ui/AppText";
import { SymbolView } from "../../components/ui/AppSymbol";
import * as Dialog from "../../components/ui/Dialog";
import { formatBytes } from "../../lib/formatBytes";
import { useQuietThreadShell } from "../../state/entities";
import { orchestrationEnvironment } from "../../state/orchestration";
import { hostPressureAlertAtom, type StrainedHost } from "../../state/presentation";
import { useAtomCommand } from "../../state/use-atom-command";
import { useOpenLinkedThread } from "../threads/useOpenLinkedThread";
import { hostPressureAgentUsage } from "./hostPressureAgentUsage";
import { kindTitle, machinesTitle } from "./hostPressureTitles";

type GT = ReturnType<typeof useGT>;

/**
 * A strained machine's warning, at the top of its Environment page. The header's warning pill
 * leads here, so the thread list stays clear of a banner.
 */
export function HostPressureDetails(props: { readonly environmentId: EnvironmentId }) {
  const alert = useAtomValue(hostPressureAlertAtom);
  const entry = alert?.strained.find((host) => host.environmentId === props.environmentId);
  if (entry === undefined) return null;
  return (
    <View
      accessibilityRole="summary"
      className="gap-2 rounded-2xl border border-border bg-card px-3.5 py-3"
    >
      <SingleMachineDetails entry={entry} />
    </View>
  );
}

/**
 * Every strained machine, at the top of the Environments list, where the header's warning pill
 * leads when more than one is strained. Each machine opens its own page.
 */
export function HostPressureMachines(props: {
  readonly onOpenEnvironment: (environmentId: EnvironmentId) => void;
}) {
  const alert = useAtomValue(hostPressureAlertAtom);
  if (alert === null || alert.strained.length < 2) return null;
  return (
    <View
      accessibilityRole="summary"
      className="mb-6 gap-2 rounded-2xl border border-border bg-card px-3.5 py-3"
    >
      <MachinesDetails strained={alert.strained} onOpenEnvironment={props.onOpenEnvironment} />
    </View>
  );
}

function WarningTitle(props: { readonly title: string }) {
  return (
    <View className="flex-row items-center gap-2">
      <SymbolView
        name="exclamationmark.triangle"
        size={14}
        tintColorClassName="accent-warning-foreground"
      />
      <AppText textRole="footnote" className="min-w-0 flex-1 font-oni-medium text-foreground">
        {props.title}
      </AppText>
    </View>
  );
}

/** Without a single machine's reading, the disk explanation leaves out the free space. */
function kindExplanation(gt: GT, kind: HostPressureKind, pressure: HostPressure | null): string {
  switch (kind) {
    case "memory":
      return gt(
        "Agents keep waiting while the system frees up memory, so they may stop responding. Stopping the largest agent helps the rest.",
      );
    case "disk":
      return pressure === null || pressure.diskFreeBytes === null
        ? gt("Agents fail once the disk is full. Remove worktrees you no longer need.")
        : gt(
            "{free} left where Onicode keeps its data and worktrees. Agents fail once it is full. Remove worktrees you no longer need.",
            { free: formatBytes(pressure.diskFreeBytes) },
          );
    case "cpu":
      return gt("Work waits for a free CPU most of the time, so agents and builds run slowly.");
    case "io":
      return gt("Work waits on disk reads and writes most of the time, so agents run slowly.");
  }
}

function SingleMachineDetails(props: { readonly entry: StrainedHost }) {
  const gt = useGT();
  const { pressure } = props.entry;
  return (
    <>
      {severeHostPressureKinds(pressure).map((kind) => (
        <View key={kind} className="gap-1">
          <WarningTitle title={kindTitle(gt, kind, null)} />
          <AppText textRole="caption" className="text-muted-foreground">
            {kindExplanation(gt, kind, pressure)}
          </AppText>
        </View>
      ))}
      <AgentRows entry={props.entry} />
    </>
  );
}

/** Each problem is explained once; each machine then lists its own problems and agents. */
function MachinesDetails(props: {
  readonly strained: ReadonlyArray<StrainedHost>;
  readonly onOpenEnvironment: (environmentId: EnvironmentId) => void;
}) {
  const gt = useGT();
  const kinds = (["memory", "disk", "cpu", "io"] as const).filter((kind) =>
    props.strained.some((entry) => entry.pressure[kind] === "severe"),
  );
  return (
    <>
      <WarningTitle title={machinesTitle(gt, props.strained)} />
      {kinds.map((kind) => (
        <View key={kind} className="gap-1">
          {kinds.length > 1 ? (
            <AppText textRole="footnote" className="text-foreground">
              {kindTitle(gt, kind, null)}
            </AppText>
          ) : null}
          <AppText textRole="caption" className="text-muted-foreground">
            {kindExplanation(gt, kind, null)}
          </AppText>
        </View>
      ))}
      {props.strained.map((entry) => (
        <View key={entry.environmentId} className="gap-2 border-t border-border pt-2">
          <Pressable
            accessibilityRole="link"
            accessibilityLabel={entry.label}
            onPress={() => props.onOpenEnvironment(entry.environmentId)}
            className="flex-row items-center gap-2 active:opacity-70"
          >
            <View className="min-w-0 flex-1 gap-0.5">
              <AppText
                textRole="footnote"
                numberOfLines={1}
                className="font-oni-medium text-foreground"
              >
                {entry.label}
              </AppText>
              {kinds.length > 1 ? (
                <AppText textRole="caption" className="text-muted-foreground">
                  {severeHostPressureKinds(entry.pressure)
                    .map((kind) => kindTitle(gt, kind, null))
                    .join(" · ")}
                </AppText>
              ) : null}
            </View>
            <SymbolView
              name="chevron.right"
              size={14}
              tintColorClassName="accent-chevron"
              type="monochrome"
            />
          </Pressable>
          <AgentRows entry={entry} />
        </View>
      ))}
    </>
  );
}

function AgentRows(props: { readonly entry: StrainedHost }) {
  const rankedBy = hostPressureRanking(props.entry.pressure);
  return props.entry.pressure.heaviestAgents.map((agent, index) => (
    <HostPressureAgentRow
      key={agent.threadId ?? `${agent.agent}:${index}`}
      environmentId={props.entry.environmentId}
      agent={agent}
      rankedBy={rankedBy}
      canForceStop={props.entry.canForceStop}
    />
  ));
}

function HostPressureAgentRow(props: {
  readonly environmentId: EnvironmentId;
  readonly agent: HostPressureAgent;
  readonly rankedBy: "memory" | "cpu";
  readonly canForceStop: boolean;
}) {
  const gt = useGT();
  const threadId = props.agent.threadId;
  const thread = useQuietThreadShell(
    threadId === null ? null : scopeThreadRef(props.environmentId, threadId),
  );
  const openThread = useOpenLinkedThread();
  const permitted = useAtomValue(
    orchestrationEnvironment.forceStopThread.permissionAtom(props.environmentId),
  );
  const forceStopThread = useAtomCommand(orchestrationEnvironment.forceStopThread, {
    reportFailure: false,
  });
  const [stopping, setStopping] = useState(false);
  const name =
    thread?.title ??
    (threadId === null ? gt("{agent} (shared)", { agent: props.agent.agent }) : props.agent.agent);
  const usage = hostPressureAgentUsage(gt, props.agent, props.rankedBy);
  const forceStop = async (target: NonNullable<typeof threadId>, runId: RunId | null) => {
    setStopping(true);
    const result = await forceStopThread({
      environmentId: props.environmentId,
      input: runId === null ? { threadId: target } : { threadId: target, runId },
    });
    setStopping(false);
    if (result._tag === "Failure" && !isAtomCommandInterrupted(result)) {
      Dialog.alert(gt("Could not force stop the agent"));
    }
  };
  return (
    <View className="flex-row items-center gap-3">
      <Pressable
        accessibilityRole={threadId === null ? undefined : "link"}
        disabled={threadId === null}
        onPress={() =>
          threadId === null
            ? undefined
            : openThread({ environmentId: props.environmentId, threadId })
        }
        className="min-w-0 flex-1 active:opacity-70"
      >
        <AppText textRole="footnote" numberOfLines={1} className="text-foreground">
          {name}
        </AppText>
      </Pressable>
      <AppText textRole="footnote" className="text-muted-foreground tabular-nums">
        {usage}
      </AppText>
      {threadId !== null && props.canForceStop && permitted ? (
        <Pressable
          accessibilityRole="button"
          accessibilityLabel={gt("Force stop {name}", { name })}
          disabled={stopping}
          hitSlop={8}
          onPress={() => {
            // The server targets the active run, else the latest that started, and refuses a
            // stale id as superseded. Binding it means a run started while the dialog was open
            // is never the one stopped; a thread that never ran is stopped by thread alone.
            const latestRun = thread?.latestRun ?? null;
            const runId =
              latestRun !== null && latestRun.status !== "queued" ? latestRun.runId : null;
            Dialog.confirm({
              title: gt("Force stop the agent?"),
              message: gt("Force stop kills {name}''s processes and ends its turn.", { name }),
              confirmText: gt("Force stop"),
              destructive: true,
              onConfirm: () => void forceStop(threadId, runId),
            });
          }}
          className={`active:opacity-70 ${stopping ? "opacity-50" : ""}`}
        >
          <AppText textRole="footnote" className="font-oni-medium text-danger-foreground">
            {stopping ? gt("Stopping...") : gt("Force stop")}
          </AppText>
        </Pressable>
      ) : null}
    </View>
  );
}
