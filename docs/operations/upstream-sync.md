# Syncing from t3code

Onicode is a fork of [t3code](https://github.com/pingdotgg/t3code), kept as the `upstream` remote. We take upstream fixes and features one reviewed commit at a time. Never `git merge upstream/main`. The rebrand (`@t3tools` → `@onicode`, `T3CODE_` → `ONICODE_`, `t3` paths → `oni`), the removed T3 services (Connect, relay, Clerk, telemetry, the hosted web app, the marketing site) and Onicode's own releases mean a blind merge brings back what we removed and quietly overwrites what we changed.

```sh
git fetch upstream
node scripts/upstream-sync.ts pending        # upstream commits not yet taken or skipped
node scripts/upstream-sync.ts show <sha>     # the commit, translated into Onicode's names
node scripts/upstream-sync.ts port <sha>     # 3-way merge of each file into the working tree
node scripts/upstream-sync.ts record <sha> take|skip "<Onicode commit, or why skipped>"
```

`port` translates the commit's before and after versions through the same rules the rebrand used, then merges each file against ours with `git merge-file`, so context that matches is applied and real divergence becomes a conflict. It reports each file as `merged`, `CONFLICT`, `MISSING` (upstream changed a file Onicode removed) or `deleted`. Port one commit, commit it, then port the next, oldest first.

For every commit:

1. Read it with `show` and decide. Skip anything that only serves T3's services or T3's release pipeline (`pending` flags commits whose paths are all in removed areas, but read them anyway). Take product fixes and features.
2. After `port`, read the whole `git diff`, not only the conflicts. A clean merge can still call a module we deleted, re-add a T3 URL, or put back a nightly-channel branch. `git diff | grep -nE '(^|[^A-Za-z0-9])[Tt]3([^0-9]|$)|pingdotgg|telemetry'` is a quick check for names the translation missed.
3. Resolve `CONFLICT` files by hand. For `MISSING`, check whether the change matters for something Onicode kept somewhere else; usually it doesn't, and the commit is taken without that file.
4. Typecheck the affected packages and run the tests beside the change, as for any other change.
5. Commit with the upstream subject and a `Ported from pingdotgg/t3code@<sha>.` line, then `record` it. Record skips too, with the reason, so nobody reviews the same commit twice. The ledger is `scripts/upstream-sync.ledger.tsv`.
