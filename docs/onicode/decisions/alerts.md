# Agent alerts: the engine decides, every viewer shows

When an agent needs its owner, the owner hears about it on the device in front of them, and can approve,
deny, reply or stop from the notification. There is no relay and no push service.

## Who decides
- The engine that runs the agent decides its alerts (`onicode_engine::alerts`). It outlives the window, so
  alerts keep coming with every window closed. The engine's own hubs are the truth (engine.md, Authority):
  the workspace (titles, marks, whether a turn runs), the session of each running turn (its approvals,
  questions and status), and every terminal program's OSC 7501 status (`TerminalHub::statuses`).
- The decision is a pure machine, `Alerter`, on a clock the caller passes. Its result is served to every
  viewer as `WatchAlerts`, on the same topic, cursor and backpressure as every other watch. The app
  presents it today; the phone takes the same watch with step 13.

## Which events, and no noise
- Four causes only: the agent **needs you** (an approval or a question, or a terminal program that is
  blocked), the run **failed**, the run **finished** and nobody has looked since, or a running turn
  **stalled** (no event for 15 minutes).
- A cause is held 1.5 s before it is raised, and dropped if it no longer stands then. A dropped cause is
  never replayed, so a flicker never rings.
- One alert per chat or terminal, under one identifier (`onicode-alert:{row}`), so a newer one replaces it.
  What already stood when the engine first saw a chat or terminal (at start, after a resync) seeds silently.
- Archived chats never alert.
- A turn that ended on "Ready" while it still waited on its owner was stopped, not finished, and does not
  ring.
- Terminal programs are outside code: their words are plain text, their alerts carry no buttons, and they
  are rate-limited (one per terminal every 30 s, four across terminals a minute).

## Seen, on every device
- "Seen" is the workspace's unread mark, which every viewer of the engine reads. A viewer reports what it
  has on screen with `SetViewportFocus`. A chat on screen in a focused window never alerts, an alert it
  had is withdrawn, and the engine marks it seen (`MarkSeen`). A finished or failed alert stands only while
  the chat is unread, so it goes everywhere once any device looked.
- A report counts for 20 s, and viewers repeat it every 5 s, so a viewer that died holds nothing back.

## Showing them
- On the Mac the app posts the alerts through `UNUserNotificationCenter` in monocode's look (the app's
  name, the chat's title, the words), with one category per action set (`onicode.alert.<action ids>`),
  registered at launch. The OS banner is the only surface; nothing new is drawn in the app. The app keeps
  monocode's preferences: the Notifications switch and Sounds; each project's mutes are its engine's (below).
  Its own monocode banners for "Agent approvals and questions" and "Agent finished" are gone; the chime
  for a chat on screen stays.
- The app watches the alerts of its own engine and of every other engine whose projects the window shows,
  and reports to each one. A chat on another engine is posted under the row the app names it by
  (`peer_row`), so a click opens it here, and its buttons call the engine that raised it. A button pressed
  before an engine's first snapshot arrived (a press that launched the app) waits for it, up to 30 s.
- Until the app says it presents (or once its reports go stale), the engine posts plain banners itself
  under the app's identity, without buttons, as the app's last report allowed. As the app quits it tells
  every engine at once that it neither shows nor posts anything, so the engine's banners take over then,
  not 20 s later. A click on either opens the chat in the app.

## Project mutes: the engine keeps and applies them
- A project's notification preferences (its mute and its categories, monocode's
  `notificationPreferences.ts`) are kept by the engine that holds the project, in its settings
  (`EngineSettings::project_notifications`, by the folder as that engine names it), saved in its store
  like every engine setting. So they hold with no viewer connected and from the first moment after a
  restart: the alerts pump hands them to the hub before its first step and again whenever they move.
- The engine is the one authority: an alert a mute holds back is raised to no viewer and posted as no
  banner, so every device and the engine's own banners agree. A rule is monocode's: off for a category,
  muted until resumed, or muted until a time; an alert rings when its category is on, no mute stands, and
  it occurred after the last resume, mute end or the category turning back on. "Agent approvals and
  questions" holds back needs-you alerts; "Agent finished" holds back finished, failed and stalled. A folder
  that is not a project (empty, `/`, `~`, the home folder, or inside an `.app`) never rings.
- A viewer sets one project's preferences with `SetProjectNotifications` on the engine that holds it, so
  two viewers muting two projects never undo each other, and every viewer follows them on
  `WatchEngineSettings`. The app follows its own engine's and every engine's whose projects the window
  shows, lays them over its preferences in its own spelling (a peer project by its `peer://` URI), so
  Settings → Notifications and the project menu show each engine's mutes for its projects, and sends each
  change to the owning engine, shown at once; a call that engine fails or refuses takes the echo back. A
  change for an engine that is away waits in the forwarding engine's outbox like any mutating call, shown
  while it waits, and takes effect when that engine is back. The
  app's own reminder and linked pull-request notifications read the same preferences. `SetViewportFocus`
  carries no mutes.

## Actions
- An approval gets Approve, Deny and Stop. A question with one single-choice item gets Approve when an
  option approves, Deny (its declining option, or dismissing the question), Reply when it takes typed
  words, and Stop. Other questions get Stop only; they are answered in the chat. Finished and failed get
  Reply, which sends the next message under the session's own settings. A stall gets Stop.
- `onicode_protocol::alerts::command` turns a button into one Session call (`Decide`, `Answer`, `Stop`,
  `Send`) under a fresh `OpId`, so every viewer answers alike and a retry is safe. Buttons do not bring the
  app forward.

## The phone
- `WatchAlerts` and `SetViewportFocus` serve any viewer over the protocol. The phone presents them once
  its core is re-cut onto the protocol (engine.md, step 13). Without a push service a suspended phone hears
  nothing new; a later option is a direct-to-APNs path from the owner's own Mac with the owner's own key.

## Known gaps
- Whether the engine binary inside the bundle may post under the app's identity must be checked on a Mac.
- Time Sensitive needs an entitlement the apps do not carry yet, so a question arrives at the active level.
