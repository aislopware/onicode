import * as NodeFS from "node:fs";
import * as NodeURL from "node:url";
import { expect, it } from "vite-plus/test";

import { androidNotificationStrings } from "./androidNotificationStrings";

const AGENT_TEXT = new URL(
  "../../../modules/oni-agent-notifications/android/src/main/java/expo/modules/oniagentnotifications/AgentNotificationText.kt",
  import.meta.url,
);

it("sends every native text, with counts left for Android to fill in", () => {
  const source = NodeFS.readFileSync(NodeURL.fileURLToPath(AGENT_TEXT), "utf8");
  const native = Array.from(
    source.matchAll(/[A-Z_]+\(\s*"(\w+)",\s*"([^"]*)"/g),
    (match) => [match[1] ?? "", match[2] ?? ""] as const,
  );
  expect(androidNotificationStrings("en")).toEqual({
    locale: "en",
    ...Object.fromEntries(native),
  });
});
