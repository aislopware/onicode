import { describe, expect, it } from "@effect/vitest";

import { apnsOutcome } from "./Apns.ts";
import { fcmOutcome } from "./Fcm.ts";

describe("apnsOutcome", () => {
  it.each([
    [200, "", { outcome: "delivered" }],
    [410, '{"reason":"Unregistered"}', { outcome: "unregistered" }],
    [400, '{"reason":"BadDeviceToken"}', { outcome: "unregistered" }],
    // A sandbox token sent to production, or the other way round.
    [400, '{"reason":"DeviceTokenNotForTopic"}', { outcome: "unregistered" }],
    [400, '{"reason":"PayloadTooLarge"}', { retryable: false, reason: "PayloadTooLarge" }],
    // Apple says not to retry a refusal of the provider token.
    [
      403,
      '{"reason":"ExpiredProviderToken"}',
      { retryable: false, reason: "ExpiredProviderToken" },
    ],
    [429, '{"reason":"TooManyRequests"}', { retryable: true, reason: "TooManyRequests" }],
    [503, "<html>", { retryable: true, reason: "HTTP 503" }],
  ] as const)("%i %s", (status, body, expected) => {
    expect(apnsOutcome(status, body)).toEqual(expected);
  });
});

describe("fcmOutcome", () => {
  const fcmError = (status: string, errorCode?: string) => ({
    error: {
      status,
      details:
        errorCode === undefined
          ? []
          : [{ "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError", errorCode }],
    },
  });

  it.each([
    [200, { name: "projects/p/messages/1" }, { outcome: "delivered" }],
    [404, fcmError("NOT_FOUND", "UNREGISTERED"), { outcome: "unregistered" }],
    // The payload was wrong, which says nothing about the token.
    [
      400,
      fcmError("INVALID_ARGUMENT", "INVALID_ARGUMENT"),
      { retryable: false, reason: "INVALID_ARGUMENT" },
    ],
    [
      403,
      fcmError("PERMISSION_DENIED", "SENDER_ID_MISMATCH"),
      { retryable: true, reason: "SENDER_ID_MISMATCH" },
    ],
    [
      429,
      fcmError("RESOURCE_EXHAUSTED", "QUOTA_EXCEEDED"),
      { retryable: true, reason: "QUOTA_EXCEEDED" },
    ],
    [503, fcmError("UNAVAILABLE"), { retryable: true, reason: "UNAVAILABLE" }],
    [502, "Bad Gateway", { retryable: true, reason: "HTTP 502" }],
  ] as const)("%i %j", (status, body, expected) => {
    expect(fcmOutcome(status, body)).toEqual(expected);
  });
});
