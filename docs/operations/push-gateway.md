# Push gateway

The push gateway is the Cloudflare Worker in [`infra/push-gateway`](../../infra/push-gateway). It
holds the APNs key and the Firebase service account, and turns deliveries signed by an environment
into pushes to a phone. It stores nothing: there is no database to provision or back up.
[Push notifications](../internals/push-notifications.md) explains who it trusts and why.

A gateway serves the app builds its keys can push to. An APNs key belongs to one Apple team and a
Firebase service account to one project, so a gateway goes with one build of the mobile app, and
[building your own app](../../apps/mobile/README.md#your-own-app) means running your own gateway.
This repository names no gateway. The maintainers' gateway serves only their builds, and its
address is not here.

## Run it locally

```bash
cd infra/push-gateway
cp .dev.vars.example .dev.vars   # then fill it in
vp run dev
```

- Wrap JSON values in `.dev.vars` in single quotes. Double quotes with escapes do not parse.
- `UNATTESTED_APP_IDS` lets simulators and emulators register, since neither can attest.
- `cf dev` reads only the secrets `cloudflare.config.ts` declares, so a new setting is declared
  there before `.dev.vars` can supply it.
- Local `workerd` has no HTTP/2 client, so APNs deliveries fail locally. FCM works. Test APNs
  against a deployed gateway.

Point a dev server at it with `ONICODE_PUSH_GATEWAY_URLS=http://localhost:<port>` and a dev build of
the mobile app with `ONICODE_PUSH_GATEWAY_URL` set to the same origin. Plain `http` is accepted for
`localhost` only.

## Deploy

The gateway takes its app ids, Apple team, and hostname from the same repository-root `.env` the
mobile build reads ([`.env.example`](../../.env.example)), so the two cannot disagree. It serves
iOS when `ONICODE_APPLE_TEAM_ID` is set and Android when `ONICODE_ANDROID_GOOGLE_SERVICES_FILE`
is.

1. Collect the keys for the platforms you ship:
   - **iOS.** Turn on Push Notifications and App Attest for the App ID, and create an APNs auth
     key (`.p8`) in the Apple developer account. One key covers every app of the team.
   - **Android.** Create a Firebase project, add an Android app with your package name, and
     download its `google-services.json`. Create a service account whose only permission is
     `cloudmessaging.messages.create` and download a JSON key for it. Note the SHA-256 of each
     certificate that signs a release: your upload key's from `keytool -list -v -keystore <file>`,
     and Play's from **App integrity** when Play App Signing re-signs the app.
2. Set `ONICODE_PUSH_GATEWAY_URL` in `.env` to an `https` origin on a zone in your Cloudflare
   account. The deploy creates the DNS record and certificate for it.
3. Write the secrets to a JSON file kept outside the repository:

   | Secret                | Value                                                                                                     |
   | --------------------- | --------------------------------------------------------------------------------------------------------- |
   | `PUSH_SEALING_KEYS`   | `{"current":"k1","keys":{"k1":"<base64>"}}`, each key from `openssl rand -base64 32`                      |
   | `APNS_KEY_ID`         | The APNs auth key's id                                                                                    |
   | `APNS_PRIVATE_KEY`    | That key's `.p8` contents                                                                                 |
   | `ANDROID_APPS`        | `[{"packageName":"…","signingCertificateDigests":["<64 hex>"]}]`, the digests in lowercase without colons |
   | `FCM_SERVICE_ACCOUNT` | The service account's JSON key                                                                            |

   The file's top level maps each name to a string, so the JSON values go in as strings. Leave
   out the secrets of a platform you do not ship. Keep a copy of the sealing key: without it
   every device has to register again.

4. `vp exec cf auth login` once, then `vp run deploy --secrets-file <file>` from
   `infra/push-gateway`. A deploy keeps the secrets the Worker already has, so later ones are
   plain `vp run deploy` until a secret changes.
5. Rebuild the mobile app, which now names the gateway, and put the same origin on the list of
   each environment that should deliver through it, `ONICODE_PUSH_GATEWAY_URLS`. Set in `.env`
   when you build the desktop app or the server, it is baked in and every install of that build
   has it. A machine can also set it itself, in
   [`service.env`](../user/background-service.md#configure-the-service-environment) for a
   background service. An environment refuses devices registered with a gateway that is not on
   its list; several origins are separated by commas.

Both of the gateway's callers are programs, not browsers. Bot Fight Mode challenges them, and on
the free plan it cannot be skipped for one hostname, so put the gateway on a zone that has it off.
`workersDev` and `previewUrls` stay off so the custom domain is the only way in.

`ANDROID_BOOT_POLICY` is `verified` by default. `locked` also admits a locked bootloader running a
user-signed OS, and `any` admits unlocked devices; either weakens the proof that your app
holds the key.

## Rotate the sealing key

Add a key under a new id and make it `current`. Keep the old key in `keys` until everything it
sealed has expired, which takes 180 days, then remove it. Removing a key early signs every device
out of push until its app next opens.

## Stop everything

`vp exec cf workers secrets update PUSH_DISABLED --worker <name> --text true` refuses every request.
`vp exec cf workers secrets delete PUSH_DISABLED --worker <name>` resumes. Devices and environments
keep their tokens, so nothing needs to register again.
