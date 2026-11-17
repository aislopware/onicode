import { EnvironmentId, ThreadId } from "@onicode/contracts";
import { expect, it } from "vite-plus/test";

import {
  createDraftActivityReporter,
  DRAFT_ACTIVITY_REPORT_INTERVAL_MS,
} from "./providerSession.ts";

it("reports the first key of a draft, then at most once per interval for each thread", () => {
  const environmentId = EnvironmentId.make("environment");
  const first = { environmentId, threadId: ThreadId.make("first") };
  const second = { environmentId, threadId: ThreadId.make("second") };
  let at = 1_000;
  const reported: Array<string> = [];
  const report = createDraftActivityReporter(
    (ref) => reported.push(ref.threadId),
    () => at,
  );

  report(first);
  report(first);
  at += DRAFT_ACTIVITY_REPORT_INTERVAL_MS - 1;
  report(first);
  report(second);
  at += 1;
  report(first);
  report(second);

  expect(reported).toEqual(["first", "second", "first"]);
});
