# A server Mac stays awake while an agent runs on it

A Mac that runs agents for the owner's other devices must not fall asleep under a turn. If it does, the turn
stalls, the phone sees the computer go away, and the run resumes only when someone wakes the Mac.

- Each engine holds at most one `PreventUserIdleSystemSleep` assertion, named "Onicode: an agent or a
  terminal command is running" in `pmset -g assertions`. It holds it while any chat on this device has a turn
  open, or while a chat's terminal runs a program in the foreground. Counting the work is the reference
  count, so a second turn never takes a second assertion.
- A turn waiting on its owner (`AwaitingInput`) still holds the Mac awake: the answer may come from the phone
  minutes later, and the agent process is alive.
- It releases 10 s after the last turn or command ends, so back-to-back turns keep holding one assertion.
- Only idle system sleep is held off. The display sleeps as usual, and a closed lid or the Apple menu's
  Sleep still sleeps the Mac: Onicode never overrides the owner.
- Settings → Notifications → Power turns it off (`keepAwakeWhileAgentsRun`, on by default). The engine reads
  it within 15 s.
- The decision is the pure `onicode_engine::keep_awake::KeepAwake`, with property tests. IOKit sits behind
  `PowerAssertion`, so no test touches the machine's power state.

Not covered:
- A terminal opened outside a chat (the bare terminal panel) is not seen. The engine only asks about
  terminals it knows a chat for. The terminal lane can offer a busy count over every terminal.
- An asleep Mac can't be woken from another peer. Wake-on-LAN magic packets don't cross Tailscale (it is
  layer 3), so keeping the Mac awake while there is work is the whole answer.
