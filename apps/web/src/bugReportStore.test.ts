import { describe, expect, it } from "vite-plus/test";

import { closeBugReport, useBugReportStore } from "./bugReportStore";

describe("closeBugReport", () => {
  it("lets a late send mark its own report sent without closing a newer form", () => {
    useBugReportStore.setState({ open: true, session: 3, sentSessions: [] });
    closeBugReport(2, true);
    expect(useBugReportStore.getState()).toMatchObject({ open: true, sentSessions: [2] });

    // An older send finishing later keeps the newer one sent.
    closeBugReport(1, true);
    closeBugReport(3, false);
    expect(useBugReportStore.getState()).toMatchObject({ open: false, sentSessions: [2, 1] });
  });
});
