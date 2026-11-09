# Contributing

## Developer setup

See the [development runbook](docs/operations/development.md#first-checkout) for the initial checkout,
development commands, tests, and platform-specific desktop packaging prerequisites.

## Before you start

Report bugs and propose features in [issues](https://github.com/aislopware/onicode/issues). Search
existing issues first. For anything larger than a small fix, open an issue and agree on the direction
before writing code, so neither of us spends time on a PR that will not land.

Focused bug fixes, reliability and performance improvements, and maintenance work are the easiest
to accept. Opening a PR does not oblige us to merge it.

## Pull requests

- Solve one problem per PR. Include the contract, server, client, test, and documentation changes
  that problem needs, and send unrelated cleanups or second fixes separately.
- Use a conventional commit title in plain language, such as
  `fix(web): new threads no longer spike CPU`.
- Explain the problem, how you fixed it, how you checked it, and what you could not check. Run
  focused tests for the behavior you changed; "tests pass" alone does not show that the change works.
- UI changes need before/after screenshots, and a short recording when motion or timing matters.
  Attach them to the PR; do not commit them.
- Follow the [documentation rules](AGENTS.md#documentation) and the engineering guidance in
  [AGENTS.md](AGENTS.md). They apply to people and agents alike.

## Security

Report vulnerabilities privately as described in [SECURITY.md](.github/SECURITY.md), not in a public
issue or PR.
