import { msg } from "gt-react-native";

import { m, messageTemplate } from "../../i18n/messages";

/**
 * Copy for the Android notifications in the active language, keyed as `AgentText` in
 * AgentNotificationText.kt. Counted texts are ICU patterns: the counts arrive in a push, which
 * Android shows without JS.
 */
export function androidNotificationStrings(locale: string): Readonly<Record<string, string>> {
  return {
    locale,
    alertChannel: m(msg("Agent alerts")),
    activityChannel: m(msg("Ongoing agent activity")),

    activity: m(msg("Agent activity")),
    allFinished: m(msg("All finished", { $context: "every agent stopped working" })),
    needYou: messageTemplate(
      msg("{count, plural, one {{count} needs you} other {{count} need you}}"),
    ),
    failedCount: messageTemplate(
      msg("{count, plural, one {{count} failed} other {{count} failed}}"),
    ),
    finishedFailedCount: messageTemplate(
      msg("{count, plural, one {Finished, {count} failed} other {Finished, {count} failed}}"),
    ),
    doneCount: messageTemplate(msg("{count, plural, one {{count} done} other {{count} done}}")),
    workingCount: messageTemplate(
      msg("{count, plural, one {{count} working} other {{count} working}}"),
    ),
    activeCount: messageTemplate(
      msg("{count, plural, one {{count} active} other {{count} active}}"),
    ),
    threadCount: messageTemplate(
      msg("{count, plural, one {{count} thread} other {{count} threads}}"),
    ),

    statusConnecting: m(msg("Connecting", { $context: "thread status" })),
    statusWorking: m(msg("Working", { $context: "thread status" })),
    statusApproval: m(msg("Approval", { $context: "thread status, waiting for approval" })),
    statusInput: m(msg("Input", { $context: "thread status, waiting for user input" })),
    statusWaiting: m(msg("Waiting", { $context: "thread status" })),
    statusDone: m(msg("Done", { $context: "thread status, turn finished" })),
    statusFailed: m(msg("Failed", { $context: "thread status" })),

    // The status bar shows about seven characters.
    chipActive: m(msg("Active", { $context: "status bar chip, 7 characters" })),
    chipLive: messageTemplate(
      msg("{count} live", { count: "{count}", $context: "status bar chip, 7 characters" }),
    ),
    chipWorking: m(msg("Working", { $context: "status bar chip, 7 characters" })),
    chipApprove: m(msg("Approve", { $context: "status bar chip, 7 characters" })),
    chipAnswer: m(msg("Answer", { $context: "status bar chip, 7 characters" })),
    chipWaiting: m(msg("Waiting", { $context: "status bar chip, 7 characters" })),
    chipDone: m(msg("Done", { $context: "status bar chip, 7 characters" })),
    chipFailed: m(msg("Failed", { $context: "status bar chip, 7 characters" })),

    actionOpen: m(msg("Open", { $context: "notification button, opens the thread" })),
    actionApprove: m(msg("Approve", { $context: "notification button" })),
    actionAnswer: m(msg("Answer", { $context: "notification button" })),
    actionDismiss: m(msg("Dismiss")),
  };
}
