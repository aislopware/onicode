import type { ThreadTitleMessage } from "@onicode/provider-core/server/threadTitleContext";

export const threadTitleEvaluationCases = [
  {
    id: "referenced-thread-startup",
    source: "Maintainer report of chart thread references overriding a startup question",
    request: "Explain why one example thread failed to start and another needed a retry.",
    previousTitle: "Fix Chart Formatting",
    messages: [
      {
        role: "user",
        text: "[xlsx: chart data labels use the cells' number format](oni-context://v1/thread/thread_labels) why could this thread not start up? [Run chart format tests on external target](oni-context://v1/thread/thread_tests) why did i have to say it like this over here to get it working?",
      },
    ],
    rubric:
      "Name thread startup or prompt/retry behavior. Chart formatting and tests are titles of example threads, not the requested work. Replace the inaccurate previous title.",
  },
  {
    id: "referenced-thread-subject",
    source: "Control for maintainer report of referenced thread titles overriding the request",
    request: "Continue the actual chart formatting work in a referenced thread.",
    previousTitle: "Chart Label Number Formats",
    messages: [
      {
        role: "user",
        text: "Continue [xlsx: chart data labels use the cells' number format](oni-context://v1/thread/thread_labels). Make the chart labels inherit the cells' number formats.",
      },
    ],
    rubric:
      "Name chart label number formatting. Referenced titles can supply the subject when the user explicitly continues that work.",
  },
  {
    id: "linked-reset-credits",
    source: "https://github.com/aislopware/onicode/pull/10462",
    request: "Review the reset credit routing change.",
    previousTitle: "Review PR 10462",
    messages: [{ role: "user", text: "Review https://github.com/aislopware/onicode/pull/10462" }],
    rubric: "Name reset credit routing. Distinguish it from displaying credit balances.",
  },
  {
    id: "onboarding-merge",
    source: "https://github.com/aislopware/onicode/pull/10465",
    request: "Make onboarding one shared wizard across computers, then merge when green.",
    previousTitle: "Finish onboarding PR",
    messages: [
      { role: "user", text: "Make onboarding one shared wizard across computers." },
      {
        role: "assistant",
        text: "The wizard now handles pairing, agent selection, and project import.",
      },
      { role: "user", text: "File a PR and merge it when green." },
    ],
    rubric: "Keep the multi-computer onboarding subject. Do not title it after merging.",
  },
  {
    id: "vague-opening",
    source: "Existing lazy thread feed title scenario",
    request: "A failing test is later identified as a lazy thread feed mismatch.",
    previousTitle: "Fix failing test",
    messages: [
      { role: "user", text: "Fix this failing test." },
      {
        role: "assistant",
        text: "The lazy thread feed test expects a full message body before the client requests it.",
      },
    ],
    rubric: "Name the lazy thread feed test. Do not invent a wider mobile regression.",
  },
  {
    id: "scope-change",
    source: "Title context budget scenario",
    request: "Change the goal from QR layout to pairing expiry, despite long assistant replies.",
    previousTitle: "Improve QR layout",
    messages: [
      { role: "user", text: "Improve QR sharing layout." },
      {
        role: "user",
        text: "Change of plan. Fix pairing token expiry. Keep remote access working.",
      },
      {
        role: "assistant",
        text: "The token expires before redemption. " + "Implementation detail. ".repeat(800),
      },
      { role: "user", text: "Ship it." },
    ],
    rubric: "Name pairing expiry and honor the explicit scope change.",
  },
  {
    id: "review-umbrella",
    source: "Existing subagent monitoring title scenario",
    request: "Review subagent monitoring risks. A Codex roster issue is one finding.",
    previousTitle: "Review subagent monitoring risks",
    messages: [
      { role: "user", text: "Review subagent monitoring risks." },
      {
        role: "assistant",
        text: "One finding is a stale Codex roster. " + "Roster detail. ".repeat(800),
      },
      { role: "user", text: "Fix the findings and babysit CI." },
    ],
    rubric: "Preserve the monitoring review scope. The previous title can stay unchanged.",
  },
  {
    id: "long-opening",
    source: "Title message truncation scenario",
    request: "Investigate Android pairing while preserving the iOS flow.",
    previousTitle: "Inspect logs",
    messages: [
      {
        role: "user",
        text:
          "Investigate Android pairing. " +
          "Connection logs. ".repeat(800) +
          " Preserve the iOS pairing flow.",
      },
    ],
    rubric: "Name Android pairing. Logs are supporting evidence.",
  },
  {
    id: "research",
    source: "Maintainer title generation request",
    request: "How can we improve title generation in Onicode?",
    previousTitle: "Research title gen improvements",
    messages: [
      { role: "user", text: "How can we improve title gen further in Onicode?" },
      {
        role: "assistant",
        text: "Prioritize user messages, refine vague titles once, and resolve PR subjects.",
      },
      {
        role: "user",
        text: "Make these changes and file a PR. Babysit until everything is green.",
      },
    ],
    rubric: "Keep title generation as the subject. Do not focus on filing the PR.",
  },
  {
    id: "project-name",
    source: "https://github.com/aislopware/onicode/pull/33418",
    request: "Make hyperdrive init create a default config instead of crashing.",
    projectTitle: "Hyperdrive",
    previousTitle: "Hyperdrive Init Missing Config Crash",
    messages: [
      {
        role: "user",
        text: "hyperdrive init crashes when ~/.hyperdrive/config.toml is missing. Make hyperdrive create a default config instead of throwing.",
      },
      {
        role: "assistant",
        text: "Hyperdrive's loadConfig now writes a default config.toml when the file is absent.",
      },
    ],
    rubric: "Name the default config on init. Leave out Hyperdrive, the project name.",
  },
  {
    id: "refresh-link-opening",
    source: "Maintainer report: a thread opened with only a Slack link kept a title about the link",
    request: "Plan the work a linked Slack thread asks for.",
    automatic: true,
    previousTitle: "Slack Thread Check for Planning",
    messages: [
      {
        role: "user",
        text: "https://acme.slack.com/archives/C0123456789/p1700000000000100 check this so we can plan it",
      },
      {
        role: "assistant",
        text: "The thread asks for three changes to the mobile apps: encrypt the local key-value store, add a forced-update gate on launch, and add a maintenance mode the backend can switch on. I drafted a plan with the storage encryption first, since the other two read from that store.",
      },
    ],
    rubric:
      "Name the planned work: storage encryption, forced update, maintenance mode, or an umbrella for them. A title that still speaks of a Slack thread or link fails.",
  },
  {
    id: "refresh-keeps-accurate",
    source: "Control for automatic refresh: most refreshes reworded a title that was already right",
    request: "Fix a window flicker, then review, merge, and release the fix.",
    automatic: true,
    previousTitle: "Fix Intermittent Window Flicker",
    messages: [
      {
        role: "user",
        text: "The whole window flickers once in a while, for example while scrolling. I cannot reproduce it on demand. Find the cause and fix it.",
      },
      {
        role: "assistant",
        text: "The flicker comes from the compositor dropping the backing layer when a sidebar row animates opacity during a scroll. I moved that animation to a transform and the layer now stays alive.",
      },
      { role: "user", text: "Open a PR and fix whatever the review bot finds." },
      {
        role: "assistant",
        text: "The PR is open. The bot found one unused import, which I removed. All checks pass.",
      },
      { role: "user", text: "Merge it and cut a release." },
    ],
    rubric:
      "Return the previous title unchanged, character for character. The thread is still about the window flicker; a reworded or sharpened title fails.",
  },
  {
    id: "refresh-subject-moved",
    source: "Maintainer report: a thread kept its first title after the user changed the topic",
    request: "A release-readiness question turns into a Live Activity redesign.",
    automatic: true,
    previousTitle: "Notification Status in Current Release",
    messages: [
      { role: "user", text: "If we release the current build, do notifications work yet?" },
      {
        role: "assistant",
        text: "Yes. Push delivery and tap-through both work in the current build.",
      },
      {
        role: "user",
        text: "Good. Different thing now: the Live Activity looks cramped. Redesign it with our own status colours and uniform rows.",
      },
      {
        role: "assistant",
        text: "The Live Activity now uses a black card, one row per working thread, and status colours that match the app.",
      },
      { role: "user", text: "Make the rows the same height and show me captures of each state." },
    ],
    rubric:
      "Name the Live Activity redesign. The release-readiness question was answered and left behind, so keeping the previous title fails.",
  },
  {
    id: "refresh-missed-subject",
    source: "Maintainer report: a ticket-check title outlived the crash fix the thread became",
    request: "A check of a ticket's remaining work becomes a crash fix.",
    automatic: true,
    previousTitle: "ACME-164 Remaining Work Check",
    messages: [
      {
        role: "user",
        text: "https://tracker.acme.example/projects/ACME/work_packages/ACME-164 what is still open in this ticket?",
      },
      {
        role: "assistant",
        text: "Two crashes from the 5.1.8 crash report are still open in ACME-164: a null place id in address autocomplete, and a double navigation pop in checkout.",
      },
      {
        role: "user",
        text: "Start a branch for the address autocomplete crash. Reproduce it first, then fix it and add a test.",
      },
      {
        role: "assistant",
        text: "Reproduced: selecting a suggestion while the session token refreshes passes a null place id. The fix guards the selection and a new test covers the refresh race.",
      },
    ],
    rubric:
      "Name the address autocomplete crash fix. It may keep ACME-164. A title that only says a ticket is being checked fails.",
  },
  {
    id: "custom-title-scheme",
    source: "https://github.com/aislopware/onicode/pull/37565",
    request: "Relay chat to the lead agent, titled with the user's naming scheme.",
    previousTitle: "Relay chat to lead agent",
    instructions: 'Title every thread "Owner: what it does (model effort)".',
    messages: [
      {
        role: "user",
        text: "Make the relay forward every chat message to the lead agent so reviewers can see questions from subagents.",
      },
    ],
    rubric:
      'Use the user\'s "Owner: what it does (model effort)" shape with all three parts, even past the default 8-word, 40-character limit. A bare short title fails.',
  },
] satisfies ReadonlyArray<{
  id: string;
  source: string;
  request: string;
  projectTitle?: string;
  previousTitle: string;
  /** Titles the thread as the server's own refresh does, which keeps a title that is still right. */
  automatic?: boolean;
  instructions?: string;
  messages: ReadonlyArray<ThreadTitleMessage>;
  rubric: string;
}>;
