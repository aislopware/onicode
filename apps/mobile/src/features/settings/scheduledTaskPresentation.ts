import { msg } from "gt-react-native";

import { m, type MessageResolver } from "../../i18n/messages";
import { hourCycleOption } from "../threads/messageTimestamp";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

const INTERVAL_UNITS = [
  [7 * DAY, msg("Every week"), msg("{count, plural, one {{count} week} other {{count} weeks}}")],
  [DAY, msg("Every day"), msg("{count, plural, one {{count} day} other {{count} days}}")],
  [HOUR, msg("Every hour"), msg("{count, plural, one {{count} hour} other {{count} hours}}")],
  [
    MINUTE,
    msg("Every minute"),
    msg("{count, plural, one {{count} minute} other {{count} minutes}}"),
  ],
  [
    1_000,
    msg("Every second"),
    msg("{count, plural, one {{count} second} other {{count} seconds}}"),
  ],
  [
    1,
    msg("Every millisecond"),
    msg("{count, plural, one {{count} millisecond} other {{count} milliseconds}}"),
  ],
] as const;

export function formatScheduledTaskInterval(everyMs: number, resolve: MessageResolver = m): string {
  let remaining = everyMs;
  const parts: string[] = [];
  for (const [size, every, amount] of INTERVAL_UNITS) {
    const count = Math.floor(remaining / size);
    if (count === 0) continue;
    if (count === 1 && remaining === everyMs && remaining === size) return resolve(every);
    parts.push(resolve(amount, { count }));
    remaining %= size;
  }
  return resolve(msg("Every {duration}", { duration: parts.join(" ") }));
}

function clockOptions(is24Hour: boolean | undefined): Intl.DateTimeFormatOptions {
  return { hour: "numeric", minute: "2-digit", ...hourCycleOption(is24Hour) };
}

/** A schedule's `H:MM` wall time, which names no date or zone. */
export function formatScheduledTimeOfDay(timeOfDay: string, is24Hour?: boolean): string {
  const match = /^(\d{1,2}):(\d{2})$/.exec(timeOfDay);
  if (!match) return timeOfDay;
  // In UTC, so neither the device zone nor a DST gap can shift the wall time.
  return new Intl.DateTimeFormat(undefined, { ...clockOptions(is24Hour), timeZone: "UTC" }).format(
    Date.UTC(2000, 0, 1, Number(match[1]), Number(match[2])),
  );
}

export function formatScheduledRunTime(date: Date, is24Hour?: boolean): string {
  return date.toLocaleTimeString([], clockOptions(is24Hour));
}

function localCalendarDay(date: Date): number {
  // Compare calendar days, not 24-hour spans: DST days can have 23 or 25 hours.
  return Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / DAY;
}

const WEEKDAY_KEYS = [
  "sunday",
  "monday",
  "tuesday",
  "wednesday",
  "thursday",
  "friday",
  "saturday",
] as const;

export function formatNextScheduledTaskRun(
  nextRunAt: string,
  now: number,
  resolve: MessageResolver = m,
  is24Hour?: boolean,
): string {
  const next = new Date(nextRunAt);
  const current = new Date(now);
  const remaining = next.getTime() - now;
  if (!Number.isFinite(remaining)) return resolve(msg("Next run unavailable"));
  if (remaining <= 0) return resolve(msg("Next run due"));

  const days = localCalendarDay(next) - localCalendarDay(current);
  if (days === 0) {
    if (remaining < MINUTE) return resolve(msg("Next run in less than a minute"));
    const minutes = Math.round(remaining / MINUTE);
    if (minutes < 60) {
      return resolve(
        msg(
          "{count, plural, one {Next run in {count} minute} other {Next run in {count} minutes}}",
          { count: minutes },
        ),
      );
    }
    return resolve(
      msg("{count, plural, one {Next run in {count} hour} other {Next run in {count} hours}}", {
        count: Math.round(remaining / HOUR),
      }),
    );
  }

  const time = formatScheduledRunTime(next, is24Hour);
  if (days === 1) return resolve(msg("Next run tomorrow at {time}", { time }));
  if (days <= 7) {
    return resolve(
      msg(
        "{day, select, sunday {Next run next Sunday at {time}} monday {Next run next Monday at {time}} tuesday {Next run next Tuesday at {time}} wednesday {Next run next Wednesday at {time}} thursday {Next run next Thursday at {time}} friday {Next run next Friday at {time}} other {Next run next Saturday at {time}}}",
        { day: WEEKDAY_KEYS[next.getDay()] ?? "saturday", time },
      ),
    );
  }
  const date = next.toLocaleDateString([], {
    month: "short",
    day: "numeric",
    ...(next.getFullYear() !== current.getFullYear() ? { year: "numeric" as const } : {}),
  });
  return resolve(msg("Next run {date} at {time}", { date, time }));
}

const WEBHOOK_DELIVERY_LABELS: Record<string, string> = {
  dispatch_failed: msg("Run failed"),
  duplicate: msg("Duplicate", { $context: "webhook delivery outcome" }),
  coalesced: msg("Coalesced"),
  filtered: msg("Filtered", { $context: "webhook delivery outcome" }),
  rejected_signature: msg("Bad signature"),
  disabled: msg("Task paused"),
  rate_limited: msg("Rate limited"),
  expired: msg("Too old"),
};

/** What became of one webhook request. Outcomes added later show their server name. */
export function webhookDeliveryLabel(
  delivery: { readonly outcome: string; readonly pendingWindow?: boolean | undefined },
  resolve: MessageResolver = m,
): string {
  if (delivery.outcome === "accepted") {
    return delivery.pendingWindow
      ? resolve(msg("Opened window"))
      : resolve(msg("Ran", { $context: "webhook delivery outcome" }));
  }
  const label = WEBHOOK_DELIVERY_LABELS[delivery.outcome];
  return label ? resolve(label) : delivery.outcome.replaceAll("_", " ");
}

export function webhookDeliveryFailed(outcome: string): boolean {
  return outcome === "dispatch_failed" || outcome === "rejected_signature";
}
