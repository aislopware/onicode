# Agent alerts on every device, with actions

When an agent needs its owner, every device the owner uses shows a notification. From the notification the
owner can approve, deny, reply or stop without opening the chat. There is no relay and no push service.

## Who raises an alert
- Each device decides its own alerts from the registry rows it already mirrors (session status, the chat's
  synced `lastSeenAt`), with one shared machine: `onicode_proto::alerts::Alerter`. No engine sends an alert to
  a device, so there is never one alert per engine per device, and a device hears about a chat on any engine.
- On a Mac the background engine runs the machine, because it outlives the window. It knows the viewport's
  focus and the chat on screen through `SetViewportFocus`, and it reads the open question from the session
  doc. It publishes the alerts on `WatchAlerts`. The app presents them through `UNUserNotificationCenter`
  with their actions. Until the app reports that it presents them (or when its reports go stale), the
  engine posts plain banners itself, without actions or withdrawal.
- On iOS the client crate runs the same machine over its replica. Swift posts what `alerts()` returns.

## Which events, and no noise
- Four causes only: the agent **needs you** (a question or an approval), the run **failed**, the run
  **finished** and nobody has seen it yet, or its computer **stalled** mid-run. A stall counts only while
  this device is linked; otherwise the device itself is the one that is offline.
- A cause is held 1.5 s before it is raised and dropped if it no longer stands, so a flicker never rings. A
  dropped cause is never replayed.
- Each chat has one alert at a time, and a newer one replaces it under the same identifier. Chats seen for the
  first time (at boot, on reconnect, after a replay) seed silently. Side chats and archived chats never
  alert.
- A needs-you alert waits up to 4 s for its question's words, so its buttons match the question.

## Dedup across devices
- An alert is withdrawn on every device when its cause ends in the shared state. Answered on one device
  means the row leaves `AwaitingInput`. Opened on one device means the chat's `lastSeenAt` passes its finish.
  Resumed means the row is `Working` again. No device tells another to withdraw; they read the same rows.

## Focus
- No alert is raised for the chat on screen in a focused window, and one already up is withdrawn.
- With "Only when in the background" on (the default), nothing new is raised while Onicode is in front. The
  in-app dot and chime cover that case.

## Actions
- Categories: `onicode.alert.<action ids>`, one per action set, registered at launch. An answerable question
  with a clear Allow and Deny gets Approve, Deny, Reply and Stop. Otherwise it gets Reply and Stop (and
  Deny when there is one). A question with several parts or multiple choice gets Stop only, and one that
  must be answered in the app gets none. Finished and failed alerts get Reply.
- Reply is a `UNTextInputNotificationAction`. On a question it answers with the typed words. Otherwise it is
  a message with `holdForTurnEnd: false`, which steers a live turn where the agent takes it and otherwise
  starts the next turn.
- Commands ride the existing paths: on the Mac, RPC `QueueCommand` (`RespondInput`, `Interrupt`) and
  `QueueMessage`; on iOS, the session's durable command ledger. Nothing new crosses the wire.
- Buttons don't bring the app forward. Tapping the alert opens the chat.

## iOS without a push service
- What works: local notifications while the app runs, in front or in the few seconds iOS grants after it
  goes behind (plus a background task after a button, so the command reaches its computer). A button on a
  notification that is already shown works even after the app was suspended, because iOS relaunches the app
  in the background to run it.
- What does not: once iOS suspends the app, nothing new is raised. Background App Refresh runs at the
  system's whim (hours apart), so it is not a channel. An agent that asks a question while the phone sits in
  a pocket is not announced on the phone. The Macs still announce it.
- A Live Activity can be updated locally while the app runs. It would show running agents on the Lock
  Screen, but it can't outlive the app's suspension without push updates either. It is a later option.
- Later option, not built: a direct-to-APNs path. The owner's Mac engine would hold the owner's own APNs key
  (`.p8`, from their developer account) and send token-based pushes to the phone's device token straight to
  Apple's APNs. There would be no Onicode server. Apple is the transport, as it is for any iPhone. It needs
  the owner's paid developer account, a build signed with their team, and the device token passed over the
  tailnet. It fits "no cloud service of ours" and would make the phone hear about questions while asleep.

## Known gaps
- Alerts delivered by an earlier run of the Mac app stay in Notification Center until their cause ends
  while the app runs again. The app does not sweep them at launch.
- Time Sensitive needs the `com.apple.developer.usernotifications.time-sensitive` entitlement. Until the
  apps carry it, a question arrives at the active level and does not break through a Focus.
