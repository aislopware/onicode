# Mobile notifications

Your phone can tell you when an agent needs approval, asks a question, finishes, or fails, and
show running agents on the Lock Screen, without the app being open or connected.

## Turn them on

On the phone, open **Settings → Notifications** and turn on **Notifications**. The setting
belongs to the phone and covers every environment the app has added, including ones you add
later. Choose which events alert you under **Notify me about**.

Each environment shows whether the phone is registered with it. Tap a registered environment to
send a test notification.

The machine running Onicode sends the notifications, so it has to be on and online. The phone
does not need to reach it at that moment.

## Follow work on the Lock Screen

**Live Activity** on iOS, **Live Updates** on Android 16 and newer, and **Ongoing notification**
on older Android show an environment's agents in the order they matter to you: the ones waiting
for you, then ones that failed, then ones still working, then ones that finished. Each
environment gets its own. A finished result stays for up to 15 minutes, or until you open its
thread on any device. What a subagent asks of you, such as an approval or a secret, shows on the
thread that started it.

If you swipe one away, it stays away until that environment has finished everything and starts
new work. iOS keeps a Live Activity for at most eight hours, so work that runs longer gets a new
one then, also after a swipe. Turning the switch off stops them and leaves alerts on.

## Alerts

A thread has at most one alert on the phone: when it has more news, the new alert takes the old
one's place. Alerts from the same environment stack together.

Opening a thread in the app clears its alert, however you got there. An alert for the thread you
are looking at in the app is not shown.

## What leaves your machine

Apple and Google only deliver pushes sent with the app's own keys, so notifications travel
through a push gateway that holds them. For each notification the gateway receives the thread
title, the project name, and the agent's status, passes them to Apple or Google, and keeps
nothing. Your machine never receives the phone's push token, and the gateway accepts
notifications for your phone only from environments the phone registered with.

With **Notifications** off, nothing is sent.

## Stop notifications for a phone you no longer have

Revoke its session under **Authorized clients** in **Settings → Sharing & access** on the host,
or with `onicode auth session revoke <id>`. A revoked phone gets nothing further.

## When they do not arrive

- **This build names no push gateway**: the app was built without one. Each build of the app
  has its own gateway, set when it is built.
- **This environment uses another push gateway**: the environment does not list the app's
  gateway. Set `ONICODE_PUSH_GATEWAY_URLS` on that machine to the gateway your app was built
  for, in [`service.env`](./background-service.md#configure-the-service-environment) for a
  background service.
- **Could not get a push token**: on Android, the app was built without a Firebase project.
- **Unreachable**: the phone could not reach the environment to register. It retries when the
  app next opens.
- On Android, pushes need Google Play services, and force-stopping the app blocks them until you
  open it again.
- Notification permission and Android notification channels are controlled in system Settings.
