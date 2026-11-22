import { describe, expect, it } from "vite-plus/test";

import { closeBugReport, useBugReportStore } from "./bugReportStore";

describe("closeBugReport", () => {
  it("lets a late send mark its own report sent without closing a newer form", () => {
    useBugReportStore.setState({ open: true, session: 2, sentSession: undefined });
    closeBugReport(1, true);
    expect(useBugReportStore.getState()).toMatchObject({ open: true, session: 2, sentSession: 1 });

    closeBugReport(2, false);
    expect(useBugReportStore.getState()).toMatchObject({ open: false, sentSession: 1 });
  });
});
