import * as NodeUtil from "node:util";

import * as HostProcess from "@onicode/shared/HostProcess";
import * as Console from "effect/Console";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import { Command, Flag, GlobalFlag, Prompt } from "effect/cli";
import { FetchHttpClient } from "effect/http";

import packageJson from "../../package.json" with { type: "json" };
import * as BootService from "../service/bootService.ts";
import * as ServiceProtection from "../service/serviceProtection.ts";
import { compareExactServiceVersions, SERVICE_ENV_FILE } from "../service/serviceProtocol.ts";
import * as ServerConfig from "../config.ts";
import * as ProcessRunner from "../processRunner.ts";
import { projectLocationFlags, resolveCliAuthConfig } from "./config.ts";
import { resolveEntryPath } from "./invocation.ts";

export const layer = (config: ServerConfig.ServerConfig["Service"]) =>
  BootService.layer({
    baseDir: config.baseDir,
    logsDir: config.logsDir,
    cliVersion: packageJson.version,
  }).pipe(Layer.provide(ProcessRunner.layer), Layer.provide(FetchHttpClient.layer));

export type ServiceReconcileResult =
  | {
      readonly changed: false;
      readonly status: BootService.BootServiceStatus;
    }
  | {
      readonly changed: true;
      readonly previouslyInstalled: boolean;
      readonly plan: BootService.BootServicePlan;
    };

const SERVICE_ENV_KEY = /^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=/u;

/**
 * Sets or removes (`undefined`) entries in `service.env` text and keeps every
 * other line, including comments, as the user wrote it.
 */
export const upsertServiceEnv = (
  contents: string,
  entries: Readonly<Record<string, string | undefined>>,
): string => {
  const written = new Set<string>();
  const lines = contents.split("\n").flatMap((line) => {
    const key = SERVICE_ENV_KEY.exec(line)?.[1];
    if (key === undefined || !Object.hasOwn(entries, key)) return [line];
    const value = entries[key];
    if (value === undefined || written.has(key)) return [];
    written.add(key);
    return [`${key}=${value}`];
  });
  while (lines.at(-1) === "") lines.pop();
  for (const [key, value] of Object.entries(entries)) {
    if (value !== undefined && !written.has(key)) lines.push(`${key}=${value}`);
  }
  return lines.length === 0 ? "" : `${lines.join("\n")}\n`;
};

/** `service.env` under `baseDir`, parsed as the service launcher parses it. */
export const readServiceEnv = Effect.fn("cli.service.read_env")(function* (baseDir: string) {
  const fs = yield* FileSystem.FileSystem;
  const path = (yield* Path.Path).join(baseDir, SERVICE_ENV_FILE);
  const contents = (yield* fs.exists(path)) ? yield* fs.readFileString(path) : "";
  const entries = Object.entries(NodeUtil.parseEnv(contents)).flatMap(([key, value]) =>
    value === undefined ? [] : [[key, value] as const],
  );
  return { path, contents, entries: Object.fromEntries(entries) };
});

export const writeServiceEnv = Effect.fn("cli.service.write_env")(function* (
  baseDir: string,
  entries: Readonly<Record<string, string | undefined>>,
) {
  const fs = yield* FileSystem.FileSystem;
  const current = yield* readServiceEnv(baseDir);
  const next = upsertServiceEnv(current.contents, entries);
  if (next === current.contents) return;
  yield* fs.makeDirectory(baseDir, { recursive: true });
  yield* fs.writeFileString(current.path, next);
});

export const reconcileService = Effect.fn("cli.service.reconcile")(function* (options?: {
  readonly allowDowngrade?: boolean;
  readonly start?: boolean;
}) {
  const { baseDir } = yield* ServerConfig.ServerConfig;
  const service = yield* BootService.BootService;
  const status = yield* service.status;
  // Before the downgrade check, which reads this home's state, not the service's.
  if (status.otherBaseDir !== undefined) {
    return yield* new BootService.BootServiceOtherBaseDirError({
      installedBaseDir: status.otherBaseDir,
      baseDir,
    });
  }
  if (status.installed && status.current) {
    return { changed: false, status } satisfies ServiceReconcileResult;
  }
  if (status.supported && !status.installed) {
    // A new service listens on this machine only, until `onicode up` opens the local network.
    const { entries } = yield* readServiceEnv(baseDir);
    if (entries.ONICODE_LAN === undefined && entries.ONICODE_HOST === undefined) {
      yield* writeServiceEnv(baseDir, { ONICODE_LAN: "false" });
    }
  }
  if (
    status.installedVersion !== undefined &&
    options?.allowDowngrade !== true &&
    compareExactServiceVersions(packageJson.version, status.installedVersion) < 0
  ) {
    return yield* new BootService.BootServiceDowngradeRefusedError({
      installedVersion: status.installedVersion,
      targetVersion: packageJson.version,
    });
  }
  const plan = yield* service.install(options);
  return {
    changed: true,
    previouslyInstalled: status.installed,
    plan,
  } satisfies ServiceReconcileResult;
});

export function formatServiceChange(
  result: Extract<ServiceReconcileResult, { readonly changed: true }>,
): string {
  if (result.plan.handedOff === true) {
    return `Service update to onicode@${packageJson.version} queued. This session will disconnect.\nLog: ${result.plan.logPath}`;
  }
  return `Service ${result.previouslyInstalled ? "updated" : "installed"} · onicode@${packageJson.version}\nLog: ${result.plan.logPath}`;
}

function formatProtection(
  protection: ServiceProtection.ServiceProtectionState | undefined,
): ReadonlyArray<string> {
  if (protection === undefined) return [];
  return [
    protection.unprotected === undefined
      ? `  Memory protection: on${protection.setByProtect ? " (onicode service protect)" : ""}`
      : `  Memory protection: off, ${protection.unprotected} grants none. Run \`onicode service protect\`.`,
    ...(protection.oomScoreAdj === undefined ? [] : [`  OOM score: ${protection.oomScoreAdj}`]),
  ];
}

function formatServiceStatus(
  status: BootService.BootServiceStatus,
  cliVersion: string,
  protection?: ServiceProtection.ServiceProtectionState,
): string {
  if (!status.supported) {
    return "Onicode service\n  Status: unavailable on this machine\n  Supported on: Linux with systemd, macOS with launchd";
  }
  if (!status.installed) {
    return "Onicode service\n  Status: not installed\n  Next: Run `onicode service install`.";
  }
  if (status.otherBaseDir !== undefined) {
    return [
      "Onicode service",
      "  Status: installed for another Onicode home",
      `  Base dir: ${status.otherBaseDir}`,
      `  Next: Pass \`${BootService.formatBootServiceBaseDirOption(status.otherBaseDir)}\` to manage it.`,
    ].join("\n");
  }
  const installedVersion = status.installedVersion ?? cliVersion;
  const problems = (status.problems ?? []).map(
    (problem) => `  [${problem}] ${BootService.formatBootServiceProblem(problem)}`,
  );
  if (
    !status.current &&
    status.installedVersion !== undefined &&
    compareExactServiceVersions(status.installedVersion, cliVersion) > 0
  ) {
    return [
      "Onicode service",
      `  Status: installed · onicode@${installedVersion} (newer than this onicode@${cliVersion} CLI)`,
      `  Unit: ${status.unitPath}`,
      `  Logs: ${status.logPath}`,
      ...formatProtection(protection),
      ...problems,
      `  Next: Run \`onicode update ${installedVersion}\` to match it, or pass \`--allow-downgrade\` to \`onicode service install\` explicitly.`,
    ].join("\n");
  }
  return [
    "Onicode service",
    `  Status: ${status.current ? `installed · onicode@${installedVersion}` : "needs an update or repair"}`,
    `  Unit: ${status.unitPath}`,
    `  Logs: ${status.logPath}`,
    ...formatProtection(protection),
    ...problems,
    ...(status.current ? [] : ["  Next: Run `onicode service install` to repair it."]),
  ].join("\n");
}

const runServiceCommand = Effect.fn("cli.service.run")(function* <A, E, R>(
  flags: { readonly baseDir: Parameters<typeof resolveCliAuthConfig>[0]["baseDir"] },
  run: Effect.Effect<A, E, R>,
) {
  const logLevel = yield* GlobalFlag.LogLevel;
  const config = yield* resolveCliAuthConfig(flags, logLevel);
  return yield* run.pipe(Effect.provide(Layer.merge(layer(config), ServerConfig.layer(config))));
});

const serviceReconcileFlags = {
  ...projectLocationFlags,
  allowDowngrade: Flag.Boolean("allow-downgrade").pipe(
    Flag.withDescription("Allow replacing a newer installed service with this older CLI version."),
    Flag.withDefault(false),
  ),
};

const serviceInstallCommand = Command.make("install", serviceReconcileFlags).pipe(
  Command.withDescription(
    "Install Onicode as a background service for this user. `onicode up` also chooses how your other devices reach it.",
  ),
  Command.withHandler((flags) =>
    runServiceCommand(
      flags,
      Effect.gen(function* () {
        const result = yield* reconcileService({ allowDowngrade: flags.allowDowngrade });
        if (!result.changed) {
          yield* Console.log(
            `Onicode service is already installed with onicode@${packageJson.version} (${yield* resolveEntryPath}).`,
          );
          return;
        }
        yield* Console.log(formatServiceChange(result));
        if (!result.previouslyInstalled) {
          yield* Console.log("Run `onicode up` to choose how your other devices reach it.");
        }
      }),
    ),
  ),
);

const serviceUpdateCommand = Command.make("update", serviceReconcileFlags).pipe(
  Command.withDescription("Deprecated. Run `onicode update` to move to a newer release."),
  Command.unlisted,
  Command.withHandler((flags) =>
    runServiceCommand(
      flags,
      Effect.gen(function* () {
        yield* Console.log(
          "`onicode service update` is deprecated: run `onicode update` to move to a newer release, or `onicode service install` to repair the service. Repairing now.",
        );
        const result = yield* reconcileService({ allowDowngrade: flags.allowDowngrade });
        if (!result.changed) {
          yield* Console.log(
            `Onicode service is already using onicode@${packageJson.version} (${yield* resolveEntryPath}).`,
          );
          return;
        }
        yield* Console.log(formatServiceChange(result));
      }),
    ),
  ),
);

const serviceRestartCommand = Command.make("restart", projectLocationFlags).pipe(
  Command.withDescription(
    "Restart the background service. Picks up a version installed by `onicode update` that was not restarted at the time.",
  ),
  Command.withHandler((flags) =>
    runServiceCommand(
      flags,
      Effect.gen(function* () {
        const service = yield* BootService.BootService;
        const status = yield* service.status;
        const outcome = yield* service.restart;
        const onVersion =
          status.installedVersion === undefined ? "" : ` on onicode@${status.installedVersion}`;
        yield* Console.log(
          outcome === "restarted"
            ? `Restarted the Onicode service${onVersion}.`
            : outcome === "queued"
              ? `Queued a restart of the Onicode service${onVersion}. This session runs inside the service and disconnects when it goes down.\nLogs: ${status.logPath}`
              : status.otherBaseDir !== undefined
                ? `The Onicode service serves ${status.otherBaseDir}, so it was not restarted. Pass \`${BootService.formatBootServiceBaseDirOption(status.otherBaseDir)}\` to restart it.`
                : "Onicode service is not installed.",
        );
      }),
    ),
  ),
);

const serviceUninstallCommand = Command.make("uninstall", projectLocationFlags).pipe(
  Command.withDescription("Stop and remove the Onicode background service."),
  Command.withHandler((flags) =>
    runServiceCommand(
      flags,
      Effect.gen(function* () {
        const service = yield* BootService.BootService;
        const path = yield* Path.Path;
        const { baseDir } = yield* ServerConfig.ServerConfig;
        // An unreadable unit stays removable; only a unit known to serve another home is kept.
        const installedBaseDir = yield* service.status.pipe(
          Effect.map((status) => status.installedBaseDir),
          Effect.orElseSucceed(() => undefined),
        );
        if (
          installedBaseDir !== undefined &&
          path.resolve(installedBaseDir) !== path.resolve(baseDir)
        ) {
          yield* Console.log(
            `The Onicode service serves ${installedBaseDir}, so it was not removed. Pass \`${BootService.formatBootServiceBaseDirOption(installedBaseDir)}\` to remove it.`,
          );
          return;
        }
        const removed = yield* service.uninstall;
        yield* Console.log(
          removed ? "Removed the Onicode service." : "Onicode service is not installed.",
        );
      }),
    ),
  ),
);

const serviceStatusCommand = Command.make("status", projectLocationFlags).pipe(
  Command.withDescription("Show whether the Onicode background service is installed."),
  Command.withHandler((flags) =>
    runServiceCommand(
      flags,
      Effect.gen(function* () {
        const service = yield* BootService.BootService;
        const path = yield* Path.Path;
        const status = yield* service.status;
        const protection =
          status.installed && (yield* HostProcess.Platform) === "linux"
            ? yield* ServiceProtection.readProtection(path.dirname(status.unitPath)).pipe(
                Effect.provide(ProcessRunner.layer),
              )
            : undefined;
        yield* Console.log(formatServiceStatus(status, packageJson.version, protection));
      }),
    ),
  ),
);

const formatDropIns = (dropIns: ReadonlyArray<ServiceProtection.DropIn>) =>
  dropIns.map(
    ({ path, contents }) => `  ${path}: ${contents.trimEnd().split("\n").slice(1).join(" ")}`,
  );

const confirm = (yes: boolean, message: string) =>
  yes ? Effect.succeed(true) : Prompt.run(Prompt.Confirm({ message, initial: false }));

const serviceProtectCommand = Command.make("protect", {
  ...projectLocationFlags,
  undo: Flag.Boolean("undo").pipe(
    Flag.withDescription("Remove the protection this command set up."),
    Flag.withDefault(false),
  ),
  yes: Flag.Boolean("yes").pipe(
    Flag.withDescription("Answer yes to every question, including SSH and Tailscale."),
    Flag.withDefault(false),
  ),
}).pipe(
  Command.withDescription(
    "Protect the Onicode server from agents under memory pressure (Linux with systemd). Asks for sudo.",
  ),
  Command.withHandler((flags) =>
    Effect.gen(function* () {
      const unsupported = "Service protection needs Linux with systemd.";
      if ((yield* HostProcess.Platform) !== "linux") return yield* Console.log(unsupported);
      // Before resolving config, which creates Onicode directories that root would then own.
      if ((yield* HostProcess.UserId) === 0) {
        return yield* Console.log(
          "Run `onicode service protect` as the user that runs Onicode, without sudo. It asks for sudo when it needs root.",
        );
      }
      return yield* runServiceCommand(
        flags,
        Effect.gen(function* () {
          const uid = yield* HostProcess.UserId;
          const remote = yield* ServiceProtection.detectRemoteAccessUnits;
          const { unitPath } = yield* (yield* BootService.BootService).status;
          if (uid === undefined || remote === undefined || unitPath === "") {
            return yield* Console.log(unsupported);
          }
          const userUnitDir = (yield* Path.Path).dirname(unitPath);

          if (flags.undo) {
            const installed = yield* ServiceProtection.installedProtection(
              ServiceProtection.protectionPlan({
                uid,
                userUnitDir,
                remoteAccessUnits: ServiceProtection.REMOTE_ACCESS_UNITS,
              }),
            );
            if (installed === undefined) {
              return yield* Console.log("Onicode service protection is not set up.");
            }
            yield* Console.log(
              [
                "Removes:",
                ...[...installed.system, ...installed.user, installed.managerDefaults].map(
                  ({ path, shared }) =>
                    `  ${path}${shared === true ? " (kept while another user is protected)" : ""}`,
                ),
              ].join("\n"),
            );
            if (!(yield* confirm(flags.yes, "Remove Onicode service protection?"))) return;
            yield* ServiceProtection.removeProtection(installed);
            return yield* Console.log(
              "Removed Onicode service protection. The server's OOM score returns to normal after `onicode service restart`.",
            );
          }

          yield* Console.log(
            [
              "Keeps Onicode running when agents fill this machine's memory:",
              "  - reserves memory for the Onicode server, so the kernel takes memory from agents first",
              "  - makes the out-of-memory killer pick agents before the server",
              "It adds systemd drop-in files and changes nothing else. Undo with `onicode service protect --undo`.",
            ].join("\n"),
          );
          const names = remote.map((unit) => unit.replace(/\.service$/, "")).join(" and ");
          const protectRemote =
            remote.length > 0 &&
            (yield* confirm(
              flags.yes,
              `Also protect ${names} so you can still reach this machine under memory pressure?`,
            ));
          const plan = ServiceProtection.protectionPlan({
            uid,
            userUnitDir,
            remoteAccessUnits: protectRemote ? remote : [],
          });
          yield* Console.log(
            [
              "Drop-ins it writes, as root through sudo:",
              ...formatDropIns(plan.system),
              "As you:",
              ...formatDropIns([plan.managerDefaults, ...plan.user]),
            ].join("\n"),
          );
          if (!(yield* confirm(flags.yes, "Apply these settings?"))) return;
          yield* ServiceProtection.applyProtection(plan);
          yield* Console.log(
            "Protected Onicode. Memory protection applies now; the server takes its OOM score after `onicode service restart`. Undo with `onicode service protect --undo`.",
          );
        }).pipe(Effect.provide(ProcessRunner.layer)),
      );
    }),
  ),
);

export const serviceCommand = Command.make("service").pipe(
  Command.withDescription("Manage the Onicode background service."),
  Command.withSubcommands([
    serviceInstallCommand,
    serviceRestartCommand,
    serviceUninstallCommand,
    serviceStatusCommand,
    serviceProtectCommand,
    serviceUpdateCommand,
  ]),
);
