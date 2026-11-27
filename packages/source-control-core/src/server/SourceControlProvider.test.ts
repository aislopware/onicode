import { assert, it } from "@effect/vitest";

import {
  repositoryPathFromRemoteUrl,
  transportSafeSourceControlErrorValue,
} from "./SourceControlProvider.ts";

it("removes URL credentials, query parameters, and fragments from error transport values", () => {
  assert.strictEqual(
    transportSafeSourceControlErrorValue(
      "https://user:secret@example.test/org/repo/pull/42?token=secret#discussion",
    ),
    "https://example.test/org/repo/pull/42",
  );
});

it.each([
  ["git@github.com:Onicode/onicode.git", "Onicode/onicode"],
  ["git@github.com:/Onicode/onicode.git", "Onicode/onicode"],
  ["github.com:Onicode/onicode.git", "Onicode/onicode"],
  ["github.com:/Onicode/onicode.git", "Onicode/onicode"],
  ["gitlab.com:Onicode/platform/onicode", "Onicode/platform/onicode"],
  ["hg::user@host:Owner/Repo.git", null],
  ["https://github.com/Onicode/onicode.git", "Onicode/onicode"],
  ["https://github.com/Onicode/onicode.git?view=compact", "Onicode/onicode"],
  ["https://github.com/Onicode/onicode.git#section", "Onicode/onicode"],
  ["C:/repos/onicode", null],
  ["file:///tmp/Onicode/onicode.git", null],
])("reads the repository path of %s", (remoteUrl, expected) => {
  assert.strictEqual(repositoryPathFromRemoteUrl(remoteUrl), expected);
});
