// Agent steps in a linked environment: the same three operations, as that environment's Onicode tools.
import {
  type EnvironmentId,
  OrchestratorMcpFailure,
  OrchestratorMcpThreadInterruptResult,
  OrchestratorMcpThreadReadResult,
  OrchestratorMcpThreadSendResult,
  OrchestratorMcpThreadWaitResult,
} from "@onicode/contracts";
import {
  fileBasename,
  isWindowsDrivePath,
  normalizeProjectPathForComparison,
} from "@onicode/shared/path";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Result from "effect/Result";
import * as Schedule from "effect/Schedule";
import type * as Schema from "effect/Schema";

import { ProjectToolkit } from "../mcp/toolkits/project/tools.ts";
import { refusesBeforePosition } from "../peer/PeerForwarding.ts";
import * as PeerLinks from "../peer/PeerLinks.ts";
import * as PeerMcpClient from "../peer/PeerMcpClient.ts";
import * as ServerSettings from "../serverSettings.ts";
import {
  type AgentResult,
  type AgentTurn,
  isProjectPath,
  launchOnContinue,
  WorkflowAgentError,
  type WorkflowAgents,
} from "./WorkflowAgents.ts";

// A proxy on the route may drop a request idle for about 100 s, so a wait goes out in pieces,
// no longer than the 45 s a peer waits at most.
const WAIT_PIECE = Duration.seconds(45);

/** The link, the thread or its turn's run is gone: asking again cannot help. */
const isFinal = (error: OrchestratorMcpFailure) =>
  error.code === "capability_denied" ||
  error.code === "thread_not_found" ||
  error.code === "run_not_found" ||
  error.code === "thread_not_interruptible" ||
  error.code === "invalid_request";

// The other machine asleep or a dropped route is worth a few more tries before the run pauses.
const retried = { schedule: Schedule.exponential("2 seconds"), times: 5 };

type Service = WorkflowAgents["Service"];

export const make = Effect.gen(function* () {
  const peers = yield* PeerMcpClient.PeerMcpClient;
  const links = yield* PeerLinks.PeerLinks;
  const settings = yield* ServerSettings.ServerSettingsService;
  // A run acts for whoever owns this environment, not for an agent with modes of its own, so it is
  // held to the link's access and to the administrator's ceiling here, as its steps here are.
  const limits = {
    runtimeMode: settings.managedPolicy?.limits?.maxRuntimeMode ?? "full-access",
    interactionMode: "default",
  } as const;

  const ask = <S extends Schema.Decoder<unknown>>(
    environment: string,
    tool: string,
    success: S,
    args: Record<string, unknown>,
  ) =>
    peers
      .call({
        environmentId: environment as EnvironmentId,
        tool,
        arguments: args,
        limits,
        success,
      })
      .pipe(Effect.retry({ ...retried, while: (error) => !isFinal(error) }));

  const refused = (environment: string, tool: string) =>
    Effect.mapError(
      (error: OrchestratorMcpFailure) =>
        new WorkflowAgentError({
          reason: `Environment ${environment} refused ${tool}: ${error.message}`,
          ...(isFinal(error) ? { final: true } : {}),
        }),
    );

  const call = <S extends Schema.Decoder<unknown>>(
    environment: string,
    tool: string,
    success: S,
    args: Record<string, unknown>,
  ) => ask(environment, tool, success, args).pipe(refused(environment, tool));

  const listed = Effect.fn("WorkflowAgentLinks.listed")(function* (
    environment: string,
    path: string,
  ) {
    const target = normalizeProjectPathForComparison(path);
    let cursor: number | null = 0;
    while (cursor !== null) {
      const page: Schema.Schema.Type<typeof ProjectToolkit.tools.oni_project_list.successSchema> =
        yield* call(
          environment,
          "oni_project_list",
          ProjectToolkit.tools.oni_project_list.successSchema,
          { cursor, limit: 100 },
        );
      const found = page.projects.find(
        (project) => normalizeProjectPathForComparison(project.workspaceRoot) === target,
      );
      if (found !== undefined) return found.id;
      cursor = page.nextCursor;
    }
    return undefined;
  });

  /**
   * The project at a folder there. A machine made for one run has the folder and no project yet,
   * so a folder that is not one is registered. Two steps may do that at once; the one that is
   * refused finds the other's.
   */
  const projectAt = Effect.fn("WorkflowAgentLinks.projectAt")(function* (
    environment: string,
    path: string,
  ) {
    const known = yield* listed(environment, path);
    if (known !== undefined) return known;
    const title = isWindowsDrivePath(path)
      ? fileBasename(path)
      : (path.split("/").findLast((part) => part.length > 0) ?? path);
    const created = yield* call(
      environment,
      "oni_project_create",
      ProjectToolkit.tools.oni_project_create.successSchema,
      { title, workspaceRoot: path },
    ).pipe(Effect.result);
    if (Result.isSuccess(created)) return created.success.id;
    return (yield* listed(environment, path)) ?? (yield* created.failure);
  });

  const stalled = (environment: string, threadId: string) =>
    new WorkflowAgentError({
      reason: `Environment ${environment} answered the same page of thread ${threadId} again, so the step's reply could not be read.`,
    });

  /** The whole text of the run's last assistant message. */
  const reply = Effect.fn("WorkflowAgentLinks.reply")(function* (
    turn: AgentTurn,
    environment: string,
  ) {
    const isReply = (item: OrchestratorMcpThreadReadResult["items"][number]) =>
      item.type === "assistant_message" && item.runId === turn.turnId;
    const page = { threadId: turn.threadId, view: "messages", limit: 100 };
    const readPage = (args: Record<string, unknown>) =>
      ask(environment, "oni_thread_read", OrchestratorMcpThreadReadResult, args);
    let beforePosition = Number.MAX_SAFE_INTEGER;
    // Peers older than beforePosition refuse it or ignore it; either way they read from the start.
    let read = yield* readPage({ ...page, beforePosition }).pipe(
      Effect.catchIf(refusesBeforePosition, () => readPage(page)),
      refused(environment, "oni_thread_read"),
    );
    let last = read.items.findLast(isReply);
    let earlier = read.earlierPosition;
    if (earlier !== undefined) {
      while (last === undefined && earlier !== null) {
        if (earlier >= beforePosition) return yield* stalled(environment, turn.threadId);
        beforePosition = earlier;
        read = yield* call(environment, "oni_thread_read", OrchestratorMcpThreadReadResult, {
          ...page,
          beforePosition,
        });
        last = read.items.findLast(isReply);
        earlier = read.earlierPosition ?? null;
      }
    } else {
      let afterPosition = -1;
      while (read.hasMore && read.nextPosition !== null) {
        if (read.nextPosition <= afterPosition) return yield* stalled(environment, turn.threadId);
        afterPosition = read.nextPosition;
        read = yield* call(environment, "oni_thread_read", OrchestratorMcpThreadReadResult, {
          ...page,
          afterPosition,
        });
        last = read.items.findLast(isReply) ?? last;
      }
    }
    if (last === undefined) return "";
    let text = last.text ?? "";
    let textOffset = last.textTruncated ? (last.nextTextOffset ?? null) : null;
    while (textOffset !== null) {
      const rest = yield* call(environment, "oni_thread_read", OrchestratorMcpThreadReadResult, {
        threadId: turn.threadId,
        itemId: last.itemId,
        textOffset,
        maxCharsPerItem: 50_000,
      });
      const part = rest.items[0];
      if (part === undefined) break;
      text += part.text ?? "";
      const next = part.textTruncated ? (part.nextTextOffset ?? null) : null;
      if (next !== null && next <= textOffset) return yield* stalled(environment, turn.threadId);
      textOffset = next;
    }
    return text;
  });

  const start = Effect.fn("WorkflowAgentLinks.start")(function* (
    input: Parameters<Service["start"]>[0],
    environment: string,
    requestId: string,
    here: EnvironmentId,
  ) {
    if (!(yield* links.enabled)) {
      return yield* new WorkflowAgentError({
        reason:
          "Linked environments is off on this machine, so an agent step cannot run in another one. Turn it on in Settings → Labs.",
      });
    }
    const outputSchema =
      input.outputSchema === undefined ? {} : { outputSchema: input.outputSchema };
    if (input.thread !== undefined) {
      if (input.launch !== undefined) return yield* launchOnContinue;
      const sent = yield* call(environment, "oni_thread_send", OrchestratorMcpThreadSendResult, {
        threadId: input.thread,
        message: input.prompt,
        mode: "queue",
        ...outputSchema,
        clientRequestId: requestId,
      });
      return { threadId: sent.threadId, turnId: sent.runId, environment };
    }
    if (input.project === undefined) {
      return yield* new WorkflowAgentError({
        reason: "An agent step needs a `project` to start in, or a `thread` to continue.",
      });
    }
    const projectId = isProjectPath(input.project)
      ? yield* projectAt(environment, input.project)
      : input.project;
    const launched = yield* call(
      environment,
      "oni_thread_launch",
      ProjectToolkit.tools.oni_thread_launch.successSchema,
      {
        projectId,
        title: input.title,
        ...(input.model === undefined ? {} : { modelSelection: input.model }),
        ...(input.launch?.runtimeMode === undefined
          ? {}
          : { runtimeMode: input.launch.runtimeMode }),
        ...(input.launch?.interactionMode === undefined
          ? {}
          : { interactionMode: input.launch.interactionMode }),
        ...(input.launch?.workspace === undefined
          ? {}
          : { workspaceStrategy: input.launch.workspace }),
        message: input.prompt,
        ...outputSchema,
        clientRequestId: requestId,
        // The thread there says which run it is a step of, and where that run lives.
        workflowRun: { environmentId: here, ...input.origin },
      },
    );
    if (launched.runId === null) {
      return yield* new WorkflowAgentError({
        reason: `Thread ${launched.threadId} in ${environment} started without a turn for its first message.`,
      });
    }
    return { threadId: launched.threadId, turnId: launched.runId, environment };
  });

  const result = Effect.fn("WorkflowAgentLinks.result")(function* (
    turn: AgentTurn,
    environment: string,
    options?: { readonly quiet?: Duration.Duration | undefined },
  ): Effect.fn.Return<AgentResult, WorkflowAgentError> {
    const ended = new Set(["completed", "failed", "interrupted", "cancelled", "rolled_back"]);
    // A restart there continues the turn as a new run, which the wait answers for.
    let runId = turn.turnId;
    /** The result a later turn submits, or nothing once the thread has been quiet. */
    const owed = Effect.fn("WorkflowAgentLinks.owed")(function* (quiet: Duration.Duration) {
      let left = Duration.toMillis(quiet);
      // The newest turn already seen to end without the result, so quiet is not restarted by it.
      let counted = runId;
      while (true) {
        const piece = Math.min(left, Duration.toMillis(WAIT_PIECE));
        const waited = yield* call(
          environment,
          "oni_thread_wait",
          OrchestratorMcpThreadWaitResult,
          {
            threadId: turn.threadId,
            runId,
            untilOutput: true,
            timeoutMs: piece,
          },
        );
        if (waited.output !== undefined) return Option.some(waited.output);
        // An environment that does not know `untilOutput` answers at once: nothing to wait on.
        if (!waited.timedOut) return Option.none<Schema.Json>();
        left -= piece;
        if (left > 0) continue;
        const read = yield* call(environment, "oni_thread_read", OrchestratorMcpThreadReadResult, {
          threadId: turn.threadId,
          limit: 1,
          runLimit: 1,
        });
        const latest = read.recentRuns[0];
        if (latest === undefined || latest.runId === counted) return Option.none<Schema.Json>();
        // Quiet is counted from the thread's last turn: one under way, or one not yet counted.
        if (ended.has(latest.status)) counted = latest.runId;
        left = Duration.toMillis(quiet);
      }
    });
    while (true) {
      const waited = yield* call(environment, "oni_thread_wait", OrchestratorMcpThreadWaitResult, {
        threadId: turn.threadId,
        runId,
        timeoutMs: Duration.toMillis(WAIT_PIECE),
      });
      if (waited.runId !== null) runId = waited.runId;
      if (waited.timedOut) continue;
      const read = yield* call(environment, "oni_thread_read", OrchestratorMcpThreadReadResult, {
        threadId: turn.threadId,
        limit: 1,
        runLimit: 50,
      });
      const status = read.recentRuns.find((run) => run.runId === runId)?.status;
      if (status === undefined) {
        return yield* new WorkflowAgentError({
          reason: `Thread ${turn.threadId} in ${environment} no longer lists turn ${runId}.`,
        });
      }
      if (status === "completed") {
        const output =
          waited.output !== undefined || options?.quiet === undefined
            ? Option.fromUndefinedOr(waited.output)
            : yield* owed(options.quiet);
        return {
          ending: "completed",
          detail: "",
          reply: yield* reply({ ...turn, turnId: runId }, environment),
          ...(Option.isNone(output) ? {} : { output: output.value }),
        };
      }
      if (
        status === "failed" ||
        status === "interrupted" ||
        status === "cancelled" ||
        status === "rolled_back"
      ) {
        return { ending: status === "failed" ? "failed" : "stopped", detail: status, reply: "" };
      }
      // The wait answered for a turn that has not ended. Asking again at once would spin.
      yield* Effect.sleep("5 seconds");
    }
  });

  const interrupt = (threadId: string, environment: string, runId?: string) =>
    Effect.map(
      call(environment, "oni_thread_interrupt", OrchestratorMcpThreadInterruptResult, {
        threadId,
        ...(runId === undefined ? {} : { runId }),
        reason: "Its workflow run was cancelled",
      }),
      (stopped): "stopped" | "ended" =>
        stopped.status === "interrupt_requested" ? "stopped" : "ended",
    );

  return { start, result, interrupt };
});
