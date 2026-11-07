# The app's own state

monocode kept the window layout, the preferences and a few app markers in its own store, beside the
conversations. Onicode's store belongs to the engine, which may run on another Mac, so the preferences split in two
(engine.md, step 6).

- **Engine settings** are what the engine does on its own behalf: provider CLI paths, Claude hooks, the
  background model, skills, idle agents, new-chat defaults, hidden providers, keep-awake. They live in the store
  under `engine-settings`, are served as `WatchEngineSettings`, and change one key at a time with
  `SetEngineSetting`. Every viewer sees the same values, live.
- **UI state** is what only this viewer shows: theme, layout, panels, keybindings, sounds, and the Inbox, update
  and linked-work markers. It lives in `ui.sqlite3` in the app's data directory (`crates/app/src/ui_state.rs`),
  read whole at launch and written on one thread that keeps the last value of each key, so no frame waits on disk.

**Migration, once.** At the engine's first open, it copies the engine's fields out of monocode's `workspace`
preferences into `engine-settings`. A field that fails to parse keeps its default. The app calls
`TakeViewerState` once: the engine hands over the five legacy viewer keys and forgets them. Then the app writes the
`viewer-state-adopted` marker, so neither move runs again, even when there was nothing to hand over.

**Why not one file.** A viewer on a second Mac must not inherit the first one's window layout. An engine on a
headless Mac has no theme. Keeping each half where it is used means neither side reads or writes the other's
file. The clippy boundary keeps it that way: in the app, `rusqlite::Connection` is allowed only in `ui_state.rs`.
