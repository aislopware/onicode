import type { ProviderSessionInfoOf } from "@onicode/contracts";
import * as Schema from "effect/Schema";

/** Claude's dialog control requests: the data behind its TUI `/status`, `/memory`, `/skills`… */
export type ClaudeSessionDialog =
  | "get_status"
  | "get_memory_dialog"
  | "get_skills_dialog"
  | "get_hooks_listing"
  | "get_settings"
  | "list_permission_rules";

const ClaudeStatusReply = Schema.Struct({
  sections: Schema.Array(
    Schema.Struct({
      title: Schema.String,
      rows: Schema.Array(Schema.Struct({ label: Schema.String, value: Schema.String })),
    }),
  ),
});

const ClaudeSkillsReply = Schema.Struct({
  skills: Schema.Array(
    Schema.Struct({
      name: Schema.String,
      description: Schema.optional(Schema.String),
      source: Schema.optional(Schema.String),
      tokens: Schema.optional(Schema.Number),
      state: Schema.String,
    }),
  ),
});

const ClaudeMemoryReply = Schema.Struct({
  files: Schema.Array(
    Schema.Struct({
      kind: Schema.String,
      path: Schema.String,
      label: Schema.String,
      description: Schema.optional(Schema.String),
      exists: Schema.Boolean,
    }),
  ),
  memories: Schema.optional(
    Schema.Array(
      Schema.Struct({
        name: Schema.String,
        path: Schema.String,
        description: Schema.optional(Schema.NullOr(Schema.String)),
      }),
    ),
  ),
  auto_memory: Schema.optional(Schema.Struct({ status: Schema.optional(Schema.String) })),
});

const ClaudeHooksReply = Schema.Struct({
  hooks: Schema.Array(
    Schema.Struct({
      event: Schema.String,
      matcher: Schema.String,
      sourceLabel: Schema.String,
      type: Schema.String,
      displayText: Schema.String,
      commandText: Schema.String,
      contentLabel: Schema.String,
    }),
  ),
  eventCatalog: Schema.optional(
    Schema.Array(Schema.Struct({ name: Schema.String, summary: Schema.String })),
  ),
  policy: Schema.optional(
    Schema.Struct({
      disabledByPolicy: Schema.optional(Schema.Boolean),
      allDisabled: Schema.optional(Schema.Boolean),
    }),
  ),
});

const ClaudePermissionsReply = Schema.Struct({
  state: Schema.Struct({
    rules: Schema.Array(
      Schema.Struct({
        behavior: Schema.Literals(["allow", "deny", "ask"]),
        source: Schema.String,
        rule: Schema.String,
        description: Schema.optional(
          Schema.Struct({
            prefix: Schema.optional(Schema.String),
            emphasis: Schema.optional(Schema.String),
            suffix: Schema.optional(Schema.String),
          }),
        ),
        notInEffect: Schema.optional(Schema.Boolean),
      }),
    ),
    workspaceDirectories: Schema.optional(
      Schema.Array(Schema.Struct({ path: Schema.String, source: Schema.String })),
    ),
  }),
});

const ClaudeSettingsReply = Schema.Struct({
  effective: Schema.Struct({ outputStyle: Schema.optional(Schema.String) }),
});

const ClaudeInstalledPlugins = Schema.Array(
  Schema.Struct({
    id: Schema.String,
    version: Schema.optional(Schema.String),
    scope: Schema.String,
    enabled: Schema.Boolean,
  }),
);

const ClaudeMarketplaces = Schema.Array(
  Schema.Struct({
    name: Schema.String,
    source: Schema.optional(Schema.String),
    repo: Schema.optional(Schema.String),
    url: Schema.optional(Schema.String),
    path: Schema.optional(Schema.String),
  }),
);

export const decodeClaudeStatus = Schema.decodeUnknownEffect(ClaudeStatusReply);
export const decodeClaudeSkills = Schema.decodeUnknownEffect(ClaudeSkillsReply);
export const decodeClaudeMemory = Schema.decodeUnknownEffect(ClaudeMemoryReply);
export const decodeClaudeHooks = Schema.decodeUnknownEffect(ClaudeHooksReply);
export const decodeClaudePermissions = Schema.decodeUnknownEffect(ClaudePermissionsReply);
export const decodeClaudeSettings = Schema.decodeUnknownEffect(ClaudeSettingsReply);
export const decodeClaudeInstalledPlugins = Schema.decodeUnknownEffect(
  Schema.fromJsonString(ClaudeInstalledPlugins),
);
export const decodeClaudeMarketplaces = Schema.decodeUnknownEffect(
  Schema.fromJsonString(ClaudeMarketplaces),
);
export function claudeSkillsInfo(
  reply: typeof ClaudeSkillsReply.Type,
): ProviderSessionInfoOf<"skills"> {
  return {
    _tag: "skills",
    skills: reply.skills.map((skill) => ({
      name: skill.name,
      description: skill.description ?? "",
      source: skill.source ?? "",
      ...(skill.tokens === undefined ? {} : { tokens: skill.tokens }),
      enabled: skill.state !== "off",
    })),
  };
}

export function claudeMemoryInfo(
  reply: typeof ClaudeMemoryReply.Type,
): ProviderSessionInfoOf<"memory"> {
  return {
    _tag: "memory",
    files: reply.files.map((file) => ({ ...file, description: file.description ?? "" })),
    memories: (reply.memories ?? []).map((memory) => ({
      name: memory.name,
      path: memory.path,
      description: memory.description ?? "",
    })),
    ...(reply.auto_memory?.status === undefined ? {} : { autoMemory: reply.auto_memory.status }),
  };
}

/** Only these paths may be read or written: the files Claude itself offers to edit. */
export const claudeMemoryPaths = (reply: typeof ClaudeMemoryReply.Type): ReadonlySet<string> =>
  new Set([...reply.files.map((file) => file.path), ...(reply.memories ?? []).map((m) => m.path)]);

export function claudeHooksInfo(
  reply: typeof ClaudeHooksReply.Type,
): ProviderSessionInfoOf<"hooks"> {
  const summaries = new Map((reply.eventCatalog ?? []).map((event) => [event.name, event.summary]));
  return {
    _tag: "hooks",
    hooks: reply.hooks.map((hook) => ({
      ...hook,
      eventSummary: summaries.get(hook.event) ?? "",
    })),
    disabledByPolicy: reply.policy?.disabledByPolicy === true || reply.policy?.allDisabled === true,
  };
}

export function claudePermissionsInfo(
  reply: typeof ClaudePermissionsReply.Type,
): ProviderSessionInfoOf<"permissions"> {
  return {
    _tag: "permissions",
    rules: reply.state.rules
      .filter((rule) => rule.notInEffect !== true)
      .map((rule) => {
        const description = rule.description
          ? `${rule.description.prefix ?? ""}${rule.description.emphasis ?? ""}${rule.description.suffix ?? ""}`
          : rule.rule;
        return {
          behavior: rule.behavior,
          rule: rule.rule,
          description,
          source: rule.source,
        };
      }),
    directories: reply.state.workspaceDirectories ?? [],
  };
}

export function claudePluginsInfo(
  installed: typeof ClaudeInstalledPlugins.Type,
  marketplaces: typeof ClaudeMarketplaces.Type,
): ProviderSessionInfoOf<"plugins"> {
  return {
    _tag: "plugins",
    installed: installed.map((plugin) => ({
      id: plugin.id,
      ...(plugin.version === undefined ? {} : { version: plugin.version }),
      scope: plugin.scope,
      enabled: plugin.enabled,
    })),
    marketplaces: marketplaces.map((marketplace) => ({
      name: marketplace.name,
      source: marketplace.repo ?? marketplace.url ?? marketplace.path ?? marketplace.source ?? "",
    })),
  };
}
