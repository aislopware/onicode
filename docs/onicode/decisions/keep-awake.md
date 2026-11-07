# A Mac running agents stays awake under a turn

A Mac that runs agents for its owner must not fall asleep under a turn. If it did, the turn would stall, the
owner's other devices would lose it, and the run would resume only when someone woke the Mac.

- The engine holds at most one `PreventUserIdleSystemSleep` assertion, named "Onicode: an agent or a terminal
  command is running" in `pmset -g assertions`. It holds it while any session has a turn open (the
  workspace's busy mark) or a terminal runs a program in the foreground (a job other than the shell holds it,
  or its program reports working or blocked). The work is the reference count, so a second turn never takes
  a second assertion.
- A turn waiting on its owner still holds the Mac awake: the answer may come from the phone minutes later,
  and the agent process is alive.
- It lets go 10 s after the last turn or command ends, so back-to-back turns hold one assertion.
- Only idle system sleep is held off. The display sleeps as usual, and a closed lid or the Sleep menu still
  sleeps the Mac: Onicode never overrides its owner.
- The engine setting `keep_awake_while_agents_run` (on by default) turns it off; the engine reads it every
  5 s. monocode's Settings has no control that fits it, so there is no switch until monocode grows one.
- The decision is the pure `onicode_engine::keep_awake::KeepAwake` on an injected clock, with a model
  property test. IOKit sits behind `PowerAssertion`, so no test touches the machine's power state.

Not covered: an asleep Mac can't be woken from another peer. Wake-on-LAN packets don't cross Tailscale (it is
layer 3), so keeping the Mac awake while there is work is the whole answer.
