# Mobile development lifecycle

## Hot reload and native module traps

- **HMR boundary.** The [connection runtime HMR wrapper](../../apps/mobile/src/lib/hot-swappable-atom-runtime.ts)
  swaps the Effect layer through a writable atom rather than resetting the shared atom registry;
  resetting the registry drops listeners on mounted consumers that Metro will not rerender.
- **Registry-scoped supervisors.** Environment supervisor scopes are children of the registry scope
  so interrupted startup or runtime replacement closes late arrivals instead of leaking sockets.
- **Native module `file:` copies.** Modules under `apps/mobile/modules/` are `file:` dependencies
  that pnpm copies into its virtual store. Gradle and CocoaPods compile the worktree directly, but
  Metro bundles the pnpm copy—run `vp i` after editing a local module's TypeScript or changes will
  not appear on device.

## Idle wakeups

An app that shows nothing new must not run code on every display frame (120 wakeups a second on a
120 Hz phone), and none of this shows in a render count:

- React Native's timer module runs a display link while any JS timer is due within one second.
  [`OniTimerClockSleep`](../../apps/mobile/modules/oni-native-controls/ios/OniTimerClockSleep.swift)
  sends the iOS frame callback to its own sleep timer when the next timer is more than 50 ms away.
  It reads private React Native names and leaves the stock clock alone if one is gone, so check it
  when React Native moves.
- `react-native-worklets` and Reanimated requested UI frames forever; our patches request one only
  while work waits, and Reanimated's settled-prop sync runs every 2 s. When either package moves,
  port the patch or confirm upstream stopped.

`vp run mobile:behavior --case idle-thread-wakes-nothing,ticking-label-lets-timer-clock-sleep`
checks this.

## React Compiler

Expo enables the compiler through `experiments.reactCompiler` in `app.config.ts` (not
`babel.config.js`), and release builds ship unmemoized bailouts silently (`panicThreshold: "NONE"`).
[`reactCompilerCoverage.test.ts`](../../apps/mobile/src/reactCompilerCoverage.test.ts) enforces
compilation across source files and fails on new bailouts or stale entries in `KNOWN_BAILOUTS`. When
fixing a listed file, remove its entry and manual `useMemo`/`useCallback`/`memo` in the same change
(see [Web UI](./10_web-ui.md#react-compiler) for common rewrites).

A function that compiles can still lose single caches. The same test counts uncached values in the
functions listed in `RENDER_ALLOCATION_BUDGETS` (those that render on each update of a working
thread or once per list row) and fails when a count moves; add a function when it joins that path.
Common causes: a call's result reused after a later hook, a callback called as `props.onSend()`, a
render helper called in place, and a context whose value changes on every update. A feed row must
not set state after it mounts or scrolls, since every commit walks the list's pooled rows: keep a
measured width in a ref and on-screen state in a store only its reader subscribes to.
`vp run mobile:behavior --case working-thread-renders-only-what-changed` checks the outcome.
