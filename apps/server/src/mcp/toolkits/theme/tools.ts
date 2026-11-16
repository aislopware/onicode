import {
  DefaultThemePreference,
  EnvironmentThemeFile,
  EnvironmentThemeId,
  OrchestratorMcpFailure,
} from "@onicode/contracts";
import { BUILT_IN_THEME_ID } from "@onicode/shared/themePalettes";
import * as Schema from "effect/Schema";
import { Tool, Toolkit } from "effect/ai";
import * as EnvironmentTheme from "../../../environmentTheme.ts";
import * as McpInvocationContext from "../../McpInvocationContext.ts";
import * as ThreadCommandExecutor from "../../../orchestration-v2/ThreadCommandExecutor.ts";
import * as ThreadManagementService from "../../../orchestration-v2/ThreadManagementService.ts";

const Appearance = Schema.Literals(["light", "dark", "system"]);

const shared = {
  failure: OrchestratorMcpFailure,
  failureMode: "return" as const,
  dependencies: [
    McpInvocationContext.McpInvocationContext,
    ThreadManagementService.ThreadManagementService,
    EnvironmentTheme.EnvironmentThemeService,
    ThreadCommandExecutor.ThreadCommandExecutor,
  ],
};

const ThemeReadTool = Tool.make("oni_theme_read", {
  ...shared,
  description:
    "Read Onicode's own themes and the guide to restyle it: color roles and CSS selectors. Call before `oni_theme_set`.",
  parameters: Schema.Struct({
    themeId: Schema.optionalKey(
      EnvironmentThemeId.annotate({ description: "Also return this published theme's file." }),
    ),
  }),
  success: Schema.Struct({
    current: DefaultThemePreference,
    builtIn: Schema.Array(Schema.String),
    published: Schema.Array(
      Schema.Struct({
        id: EnvironmentThemeId,
        name: Schema.String,
        hasCss: Schema.Boolean,
      }),
    ),
    theme: Schema.optionalKey(EnvironmentThemeFile),
    guide: Schema.String,
  }),
})
  .annotate(Tool.Readonly, true)
  .annotate(Tool.Destructive, false);

const ThemeSetTool = Tool.make("oni_theme_set", {
  ...shared,
  description: `Restyle Onicode itself, only when the user asks. With \`theme\`, publish it as \`themeId\` (same id updates it) and switch the desktop app running on this machine to it. Without \`theme\`, switch to a published \`themeId\`, or to \`${BUILT_IN_THEME_ID}\`, the built-in look. \`appearance\` sets light, dark, or system mode. Live full-access or default caller thread only.`,
  parameters: Schema.Struct({
    themeId: Schema.optionalKey(EnvironmentThemeId),
    theme: Schema.optionalKey(EnvironmentThemeFile),
    appearance: Schema.optionalKey(Appearance),
  }),
  success: Schema.Struct({
    themeId: Schema.optionalKey(EnvironmentThemeId),
    appearance: Schema.optionalKey(Appearance),
    published: Schema.Boolean,
  }),
}).annotate(Tool.Destructive, true);

export const ThemeToolkit = Toolkit.make(ThemeReadTool, ThemeSetTool);
