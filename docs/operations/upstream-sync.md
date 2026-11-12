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
5. `record` it, then commit with the command `port` prints, so the ledger row lands in the same commit. That command keeps the upstream subject, adds a `Ported from pingdotgg/t3code@<sha>.` line, and sets the upstream author as the commit's author with its `Co-authored-by` trailers, so the people who wrote the change keep the credit. Record skips too, with the reason, so nobody reviews the same commit twice. The ledger is `scripts/upstream-sync.ledger.tsv`.

The script reads the ledger and `HEAD` of the checkout it runs from. Run `pending` in the checkout of the branch you port onto. A checkout that is behind lists commits that branch already took.

Keep one sync PR open at a time. Port commits that arrive while it is open onto its branch instead of opening a second PR. Every PR costs a PR CI run plus a full CI run on main after it merges. Two exceptions:

- If the open PR's checks are already green, merge it first.
- If its CI run has been going a while, let it finish, then port. A push cancels the PR's in-progress CI run.
