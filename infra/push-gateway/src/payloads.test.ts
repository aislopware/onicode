import { describe, expect, it } from "@effect/vitest";
import {
  PUSH_ACTIVITY_MAX_ROWS,
  PushMessage,
  type PushActivityState,
  type PushNotification,
} from "@onicode/contracts/pushGateway";
import * as Schema from "effect/Schema";

import { utf8 } from "./bytes.ts";
import { apnsLiveActivityRequest, apnsNotificationRequest, fcmRequest } from "./payloads.ts";

// The contract caps text by character, and these characters take three bytes each.
const wide = (length: number) => "界".repeat(length);
const id = (length: number) => "t".repeat(length);
const link = `/${id(767)}`;

const notification: PushNotification = {
  id: id(43),
  title: wide(120),
  body: wide(240),
  environmentId: id(128),
  threadId: id(384),
  deepLink: link,
};
const state: PushActivityState = {
  title: wide(120),
  subtitle: wide(120),
  activeCount: PUSH_ACTIVITY_MAX_ROWS,
  updatedAt: "2026-11-12T00:00:00.000Z",
  activities: Array.from({ length: PUSH_ACTIVITY_MAX_ROWS }, (_, index) => ({
    environmentId: id(128),
    threadId: `${index}${id(383)}`,
    projectTitle: wide(120),
    threadTitle: wide(120),
    modelTitle: wide(120),
    phase: "running" as const,
    status: wide(40),
    updatedAt: "2026-11-12T00:00:00.000Z",
    deepLink: link,
  })),
};
const bytes = (value: unknown) => utf8(JSON.stringify(value)).byteLength;
const isMessage = Schema.is(PushMessage);

describe("push payloads at the contract's limits", () => {
  it("is the largest message an environment may send", () => {
    expect(
      isMessage({
        deviceId: id(128),
        environmentId: id(128),
        notification,
        activity: { state },
      }),
    ).toBe(true);
  });

  it("fits the longest alert in APNs whole", () => {
    const { payload } = apnsNotificationRequest({ notification, collapseId: "id", nowSeconds: 0 });
    expect(bytes(payload)).toBeLessThanOrEqual(4096);
    expect(payload).toMatchObject({
      aps: { alert: { title: notification.title, body: notification.body } },
      body: { threadId: notification.threadId, deepLink: link },
    });
  });

  it.each(["start", "update"] as const)(
    "shortens text, then drops the last rows, to fit a Live Activity %s",
    (event) => {
      const { payload } = apnsLiveActivityRequest({
        event,
        environmentId: id(128),
        state,
        alert: { title: wide(120), body: wide(120) },
        nowSeconds: 0,
      });
      expect(bytes(payload)).toBeLessThanOrEqual(4096);
      const shown = JSON.parse(
        (payload as { aps: { "content-state": { props: string } } }).aps["content-state"].props,
      ) as PushActivityState;
      // Ids and links cannot shrink, so the first rows survive whole and the rest go.
      expect(shown.activities.length).toBeGreaterThan(0);
      expect(shown.activities.length).toBeLessThan(PUSH_ACTIVITY_MAX_ROWS);
      expect(shown.activities[0]).toMatchObject({ threadId: `0${id(383)}`, deepLink: link });
      expect(shown.activeCount).toBe(PUSH_ACTIVITY_MAX_ROWS);
    },
  );

  it("shortens text to fit FCM without touching ids or links", () => {
    const { data } = fcmRequest({
      deviceId: id(128),
      environmentId: id(128),
      notification,
      activity: { state },
      nowMillis: 0,
    });
    expect(bytes(data)).toBeLessThanOrEqual(3800);
    expect(data).toMatchObject({
      alert_id: notification.id,
      alert_path: link,
      activity_path: link,
      alert_title: expect.stringMatching(/^界+…$/),
    });
    expect(Object.keys(data).filter((key) => key.startsWith("activity_line_"))).toHaveLength(
      PUSH_ACTIVITY_MAX_ROWS,
    );
  });
});
