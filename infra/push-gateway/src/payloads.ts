import type {
  PushActivityRow,
  PushActivityState,
  PushNotification,
} from "@onicode/contracts/pushGateway";

import { utf8 } from "./bytes.ts";

/** The widget name the app registers its Live Activity under. */
const LIVE_ACTIVITY_NAME = "AgentActivity";
/** The ActivityAttributes type expo-widgets declares; push-to-start has to name it. */
const LIVE_ACTIVITY_ATTRIBUTES_TYPE = "LiveActivityAttributes";
// Event-only updates can leave healthy agents silent for minutes.
const STALE_AFTER_SECONDS = 10 * 60;
const DISMISS_AFTER_SECONDS = 5 * 60;
// Without final content the card freezes at its last state, so it goes quickly.
const CONTENTLESS_DISMISS_AFTER_SECONDS = 15;
const APNS_MAX_PAYLOAD_BYTES = 4096;
const FCM_MAX_DATA_BYTES = 3800;
const MIN_FITTED_TEXT_LENGTH = 8;
const NOTIFICATION_TTL_SECONDS = 24 * 60 * 60;
const ACTIVITY_TTL_SECONDS = 5 * 60;

export interface ApnsRequest {
  readonly pushType: "alert" | "liveactivity";
  readonly priority: "5" | "10";
  /** Seconds since the epoch after which APNs stops trying. */
  readonly expiration: number;
  readonly collapseId?: string;
  readonly payload: unknown;
}

export interface FcmRequest {
  readonly data: Readonly<Record<string, string>>;
  readonly collapseKey?: string;
  readonly ttlSeconds: number;
}

const byteLength = (value: unknown) => utf8(JSON.stringify(value)).byteLength;

function truncate(value: string, maxLength: number): string {
  const characters = Array.from(value.trim());
  return characters.length <= maxLength
    ? characters.join("")
    : `${characters
        .slice(0, maxLength - 1)
        .join("")
        .trimEnd()}…`;
}

export function apnsNotificationRequest(input: {
  readonly notification: PushNotification;
  readonly nowSeconds: number;
}): ApnsRequest {
  const { notification } = input;
  const build = (maxLength: number) => ({
    aps: {
      alert: {
        title: truncate(notification.title, maxLength),
        body: truncate(notification.body, maxLength * 2),
      },
      sound: "default",
      "thread-id": `${notification.environmentId}/${notification.threadId}`,
    },
    // Expo exposes userInfo.body as the notification's data on iOS.
    body: {
      id: notification.id,
      environmentId: notification.environmentId,
      threadId: notification.threadId,
      deepLink: notification.deepLink,
    },
  });
  let maxLength = 120;
  let payload = build(maxLength);
  while (byteLength(payload) > APNS_MAX_PAYLOAD_BYTES && maxLength > MIN_FITTED_TEXT_LENGTH) {
    maxLength = Math.floor(maxLength * 0.8);
    payload = build(maxLength);
  }
  return {
    pushType: "alert",
    priority: "10",
    expiration: input.nowSeconds + NOTIFICATION_TTL_SECONDS,
    // APNs caps the collapse id at 64 bytes; environments send a 43-character digest.
    collapseId: notification.id.slice(0, 64),
    payload,
  };
}

export type LiveActivityEvent = "start" | "update" | "end";

/**
 * Text caps count characters, so multibyte titles can still overflow APNs' 4 KB. Shorten the text
 * first; links and ids cannot shrink, so past that drop the last, lowest-priority row.
 */
export function apnsLiveActivityRequest(input: {
  readonly event: LiveActivityEvent;
  readonly environmentId: string;
  readonly state: PushActivityState | null;
  /** `quiet` shows the banner iOS requires for a start without playing a sound. */
  readonly alert: {
    readonly title: string;
    readonly body: string;
    readonly quiet?: boolean;
  } | null;
  readonly nowSeconds: number;
}): ApnsRequest {
  const build = (maxLength: number, rowCount: number) => {
    const state =
      input.state === null
        ? null
        : {
            ...input.state,
            environmentId: input.environmentId,
            title: truncate(input.state.title, maxLength),
            subtitle: truncate(input.state.subtitle, maxLength),
            activities: input.state.activities.slice(0, rowCount).map((row) => ({
              ...row,
              projectTitle: truncate(row.projectTitle, maxLength),
              threadTitle: truncate(row.threadTitle, maxLength),
              modelTitle: truncate(row.modelTitle, maxLength),
            })),
          };
    const contentState =
      state === null
        ? {}
        : { "content-state": { name: LIVE_ACTIVITY_NAME, props: JSON.stringify(state) } };
    // An alert dictionary wakes the screen and plays a haptic instead of refreshing silently.
    const alert =
      input.alert === null
        ? {}
        : {
            alert: {
              title: truncate(input.alert.title, maxLength),
              body: truncate(input.alert.body, maxLength),
              ...(input.alert.quiet === true ? {} : { sound: "default" }),
            },
          };
    const timestamp = input.nowSeconds;
    switch (input.event) {
      case "end":
        return {
          aps: {
            timestamp,
            event: "end",
            ...contentState,
            ...alert,
            "dismissal-date":
              timestamp + (state ? DISMISS_AFTER_SECONDS : CONTENTLESS_DISMISS_AFTER_SECONDS),
          },
        };
      case "update":
        return {
          aps: {
            timestamp,
            event: "update",
            ...contentState,
            ...alert,
            "stale-date": timestamp + STALE_AFTER_SECONDS,
          },
        };
      case "start":
        return {
          aps: {
            timestamp,
            event: "start",
            ...contentState,
            "attributes-type": LIVE_ACTIVITY_ATTRIBUTES_TYPE,
            attributes: {},
            // Asks iOS 18 and later for the update token; earlier versions send it regardless.
            "input-push-token": 1,
            ...alert,
            "stale-date": timestamp + STALE_AFTER_SECONDS,
          },
        };
    }
  };

  let rowCount = input.state?.activities.length ?? 0;
  let maxLength = 120;
  let payload = build(maxLength, rowCount);
  while (
    byteLength(payload) > APNS_MAX_PAYLOAD_BYTES &&
    (rowCount > 0 || maxLength > MIN_FITTED_TEXT_LENGTH)
  ) {
    if (maxLength > MIN_FITTED_TEXT_LENGTH) {
      maxLength = Math.floor(maxLength * 0.8);
    } else {
      rowCount -= 1;
      maxLength = 120;
    }
    payload = build(maxLength, rowCount);
  }
  return {
    pushType: "liveactivity",
    priority: input.event === "update" && input.alert === null ? "5" : "10",
    expiration: input.nowSeconds + ACTIVITY_TTL_SECONDS,
    payload,
  };
}

const isAttention = (row: PushActivityRow) =>
  row.phase === "waiting_for_approval" || row.phase === "waiting_for_input";

function androidActivityData(input: {
  readonly state: PushActivityState | null;
  readonly expiresAt: number | undefined;
}): Record<string, string> {
  const rows = input.state?.activities ?? [];
  const activeCount = input.state?.activeCount ?? 0;
  const attentionCount = rows.filter(isAttention).length;
  const clean = (value: string) => value.replace(/\s+/g, " ").trim();
  const hero = rows[0];
  const title =
    activeCount > 0
      ? `${activeCount} active agent${activeCount === 1 ? "" : "s"}${
          attentionCount
            ? ` · ${attentionCount} need${attentionCount === 1 ? "s" : ""} attention`
            : ""
        }`
      : rows.some((row) => row.phase === "failed")
        ? "Agent work failed"
        : "Agent work completed";
  return {
    active: String(activeCount > 0),
    activity_chip: activeCount > 0 ? (attentionCount > 0 ? "Review" : "Active") : "",
    activity_title: title,
    activity_phase: hero?.phase ?? "",
    activity_active_count: String(activeCount),
    activity_attention_count: String(attentionCount),
    activity_body: hero
      ? `${hero.status}: ${clean(hero.threadTitle)} · ${clean(hero.projectTitle)}`
      : "",
    ...Object.fromEntries(
      rows.map((row, index) => [
        `activity_line_${index}`,
        [row.status, clean(row.threadTitle), clean(row.projectTitle)].join("\t"),
      ]),
    ),
    activity_path: hero?.deepLink ?? "/",
    activity_expires_at: String(input.expiresAt ?? 0),
  };
}

/** Keeps Unicode, escaping and several rows within FCM's 4 KB data budget. */
function fitFcmData(input: Readonly<Record<string, string>>): Record<string, string> {
  const data = { ...input };
  const textKeys = Object.keys(data).filter(
    (key) => key.endsWith("_body") || key.endsWith("_title") || key.startsWith("activity_line_"),
  );
  while (byteLength(data) > FCM_MAX_DATA_BYTES) {
    const key = textKeys.sort((a, b) => utf8(data[b]!).byteLength - utf8(data[a]!).byteLength)[0];
    if (key === undefined) break;
    const parts = key.startsWith("activity_line_") ? data[key]!.split("\t") : [data[key]!];
    const part = parts.length === 3 ? (parts[1]!.length > parts[2]!.length ? 1 : 2) : 0;
    const characters = Array.from(parts[part]!);
    // Five characters would become four plus the ellipsis and never shrink.
    if (characters.length <= 5) {
      textKeys.splice(textKeys.indexOf(key), 1);
      continue;
    }
    parts[part] = `${characters
      .slice(0, Math.floor(characters.length * 0.8))
      .join("")
      .trimEnd()}…`;
    data[key] = parts.join("\t");
  }
  return data;
}

export function fcmRequest(input: {
  readonly deviceId: string;
  readonly environmentId: string;
  readonly notification: PushNotification | undefined;
  readonly activity:
    | { readonly state: PushActivityState | null; readonly expiresAt?: number }
    | undefined;
  readonly nowMillis: number;
}): FcmRequest {
  const { notification, activity } = input;
  return {
    data: fitFcmData({
      oni_kind: "agent_activity",
      device_id: input.deviceId,
      environment_id: input.environmentId,
      updated_at: String(input.nowMillis),
      ...(notification === undefined
        ? {}
        : {
            alert_id: notification.id,
            alert_group: `${notification.environmentId}/${notification.threadId}`,
            alert_title: notification.title,
            alert_body: notification.body,
            alert_path: notification.deepLink,
          }),
      ...(activity === undefined
        ? {}
        : androidActivityData({ state: activity.state, expiresAt: activity.expiresAt })),
    }),
    // Only a silent refresh may be replaced by a later one; an alert must arrive.
    ...(notification === undefined
      ? { collapseKey: `oni-activity-${input.environmentId}`.slice(0, 64) }
      : {}),
    ttlSeconds: notification === undefined ? ACTIVITY_TTL_SECONDS : NOTIFICATION_TTL_SECONDS,
  };
}
