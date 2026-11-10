import { describe, expect, it } from "vite-plus/test";

import { translatePath, translateText } from "./upstream-sync.ts";

describe("upstream-sync translation", () => {
  it.each([
    ['import { x } from "@t3tools/contracts";', 'import { x } from "@onicode/contracts";'],
    ["process.env.T3CODE_HOME ?? T3_PORT", "process.env.ONICODE_HOME ?? ONICODE_PORT"],
    ["T3 Code keeps a t3code checkpoint", "Onicode keeps a onicode checkpoint"],
    ["new T3ConnectClient()", "new T3ConnectClient()"],
    [
      "see https://github.com/pingdotgg/t3code/pull/42",
      "see https://github.com/aislopware/onicode/pull/42",
    ],
    [
      "curl -fsSL https://t3.codes/install.sh | sh",
      "curl -fsSL https://raw.githubusercontent.com/aislopware/onicode/main/scripts/install.sh | sh",
    ],
    ["react-native-screens-5.0.0-t3.48.tgz", "react-native-screens-5.0.0-t3.48.tgz"],
    [
      "runInNewContext(`${source}\\nt3McpExtension(pi)`)",
      "runInNewContext(`${source}\\noniMcpExtension(pi)`)",
    ],
    [
      "PI_T3_MCP_EXTENSION_SOURCE ?? __T3_SETUP_DONE__",
      "PI_ONI_MCP_EXTENSION_SOURCE ?? __ONI_SETUP_DONE__",
    ],
    [
      "globalThis.__t3Recorder ?? msg_t3_turn_ ?? _t3/empty",
      "globalThis.__oniRecorder ?? msg_oni_turn_ ?? _oni/empty",
    ],
    ['first_t3="$(command -v t3)"', 'first_onicode="$(command -v onicode)"'],
    ["mcp__t3_code__delegate_task", "mcp__onicode__delegate_task"],
    [
      'T3_CHAT_THEME.id === "t3-chat" && host === "t3.chat"',
      'SAKURA_THEME.id === "sakura" && host === "t3.chat"',
    ],
    ["com.t3tools.t3code.widgets", "dev.aislopware.onicode.widgets"],
    ["java/com/t3tools/t3code/MainActivity.kt", "java/dev/aislopware/onicode/MainActivity.kt"],
  ])("%s", (upstream, onicode) => {
    expect(translateText(upstream)).toBe(onicode);
  });

  it("renames t3 paths the way the rebrand did", () => {
    expect(translatePath("apps/mobile/modules/t3-terminal/ios/T3TerminalView.swift")).toBe(
      "apps/mobile/modules/oni-terminal/ios/OniTerminalView.swift",
    );
    expect(translatePath("packages/shared/src/git.ts")).toBe("packages/shared/src/git.ts");
  });
});
