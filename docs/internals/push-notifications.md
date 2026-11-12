# Push notifications

An environment cannot push to a phone by itself: APNs and FCM accept pushes only from whoever holds
the app's keys. The [push gateway](../../infra/push-gateway) holds them. A gateway serves the builds
signed for it and nothing else: ours serves the maintainers' builds, which are not distributed
publicly, and anyone who builds the app from source runs their own. It is not a shared service. It
is still reachable by anyone and trusts no environment, so the design is about what a stranger's
server can make the app's push identity do.

## Who proves what

1. **The device proves it runs the real app.** iOS uses App Attest and Android uses hardware key
   attestation, checked against the app ids and signing certificates the gateway is configured for.
   The gateway answers with a sealed credential naming the device's key. This proves a real
   install is asking, not that the push tokens it later names are its own: it makes abuse cost a
   real phone per identity instead of a script, and deliveries are rate limited per token whichever
   grant carries it.
2. **The device chooses its environments.** It signs a request naming an environment's public key
   and its own push tokens, and gets back one sealed grant per token. The device hands the grants to
   the environment over that environment's HTTP API.
3. **The environment signs each delivery** with the key the grant names. A grant that leaks is
   useless without that key, and an environment can only reach devices that chose it.

The environment never holds a push token, only grants it cannot open. The gateway builds the APNs
and FCM payloads itself from a [typed message](../../packages/contracts/src/pushGateway.ts): an
environment decides the words of an alert and the rows of an activity, never the payload. Keep it
that way. A pass-through field is a way to send arbitrary pushes as Onicode.

## The gateway keeps no state

Challenges, credentials, and grants are sealed with the gateway's key and carry their own expiry,
so the Worker has no database, and nothing to leak but the keys. Do not add storage to it for a
feature that a field in a sealed token can carry. The cost is that a grant cannot be revoked one at
a time before it expires; an environment stops using a grant when its device is removed, and
rotating the sealing key revokes everything.

## What the environment does

[PushNotifications](../../apps/server/src/push/PushNotifications.ts) follows thread changes and
decides what each registered device should see.

- **It does no work until a device registers.** The first registration seeds its view of the
  threads from a snapshot.
- **A device belongs to the session that registered it.** Another session needs access management
  to see, test, replace, or remove it; otherwise knowing a device id would be enough to silence
  someone's phone. Devices of a revoked or expired session get nothing and are deleted at the next
  start, so revoking a phone's session is also how to stop its notifications from another client.
- **Finishing is confirmed before it is announced.** A thread that still has queued work looks
  finished for a moment between runs, so a move to finished waits five seconds for a contradiction.
- **One activity per environment.** The gateway cannot merge across environments, since it keeps
  nothing, so each environment owns one Live Activity or ongoing notification on each device.
- **A Live Activity is started by push once per stretch of work.** iOS budgets push-to-start, and
  an activity the person swiped away should stay gone until all work has stopped and new work
  begins. APNs is the only witness to a swipe and a late one: it keeps accepting updates for a
  dismissed activity, then answers `410 Unregistered` after a delay Apple does not specify. So an
  accepted update does not mean the card is on screen.

Registration is plain HTTP, not a WebSocket method, because the app registers a Live Activity's
update token from the background, where it has no socket.
