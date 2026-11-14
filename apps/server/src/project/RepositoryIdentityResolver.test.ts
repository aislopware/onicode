// @effect-diagnostics nodeBuiltinImport:off - realpathSync.native resolves Windows 8.3 short names, which the Effect realPath does not.
import * as NodeFS from "node:fs";
import * as NodeServices from "@effect/platform-node/NodeServices";
import { expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import { vi } from "vite-plus/test";

import * as ProcessRunner from "../processRunner.ts";
import { sourceControlDefinitions } from "../sourceControl/sourceControlDefinitions.ts";
import * as RepositoryIdentityResolver from "./RepositoryIdentityResolver.ts";

const normalizePathSeparators = (value: string) => value.replaceAll("\\", "/");
const normalizeResolvedPath = (value: string) => normalizePathSeparators(value);

const git = (cwd: string, args: ReadonlyArray<string>) =>
  Effect.gen(function* () {
    const processRunner = yield* ProcessRunner.ProcessRunner;
    return yield* processRunner.run({
      command: "git",
      args: ["-C", cwd, ...args],
    });
  }).pipe(Effect.provide(ProcessRunner.layer));

it.layer(NodeServices.layer)("RepositoryIdentityResolverLive", (it) => {
  it.effect("normalizes equivalent GitHub remotes into a stable repository identity", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-test-",
      });

      yield* git(cwd, ["init"]);
      yield* git(cwd, ["remote", "add", "origin", "git@github.com:Onicode/onicode.git"]);

      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      const identity = yield* resolver.resolve(cwd);
      // Native realpath matches git's long paths when Windows temp dirs use 8.3 names.
      const resolvedIdentityRoot =
        identity?.rootPath === undefined ? "" : NodeFS.realpathSync.native(identity.rootPath);
      const resolvedCwd = NodeFS.realpathSync.native(cwd);

      expect(identity).not.toBeNull();
      expect(identity?.canonicalKey).toBe("github.com/onicode/onicode");
      expect(normalizeResolvedPath(resolvedIdentityRoot)).toBe(normalizeResolvedPath(resolvedCwd));
      expect(identity?.displayName).toBe("Onicode/onicode");
      expect(identity?.provider).toBe("github");
      expect(identity?.owner).toBe("Onicode");
      expect(identity?.name).toBe("onicode");
      expect(yield* resolver.resolve(path.join(cwd, ".git"))).toBeNull();
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect("identifies the host of an SSH remote that leaves its user to ssh config", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-userless-remote-test-",
      });

      yield* git(cwd, ["init"]);
      yield* git(cwd, ["remote", "add", "origin", "github.com:Onicode/onicode.git"]);

      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      const identity = yield* resolver.resolve(cwd);

      expect(identity?.canonicalKey).toBe("github.com/onicode/onicode");
      expect(identity?.provider).toBe("github");
      expect(identity?.displayName).toBe("Onicode/onicode");
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect("keeps a literal .git in an Azure DevOps repository name", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-azure-git-suffix-test-",
      });

      yield* git(cwd, ["init"]);
      yield* git(cwd, [
        "remote",
        "add",
        "origin",
        "https://dev.azure.com/Acme/Platform/_git/Tool.git",
      ]);

      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      const identity = yield* resolver.resolve(cwd);

      expect(identity?.displayName).toBe("Acme/Platform/_git/Tool.git");
      expect(identity?.name).toBe("Tool.git");
      expect(sourceControlDefinitions.repositorySelector(identity)).toBe("Tool.git");
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect("resolves the fetch remote of a partial clone", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-partial-clone-test-",
      });

      yield* git(cwd, ["init"]);
      yield* git(cwd, ["remote", "add", "origin", "git@github.com:Onicode/onicode.git"]);
      yield* git(cwd, ["remote", "set-url", "--push", "origin", "git@github.com:Onicode/push.git"]);
      yield* git(cwd, ["config", "remote.origin.promisor", "true"]);
      yield* git(cwd, ["config", "remote.origin.partialclonefilter", "blob:none"]);

      const remotes = yield* git(cwd, ["remote", "-v"]);
      expect(remotes.stdout).toContain("(fetch) [blob:none]");

      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      const identity = yield* resolver.resolve(cwd);
      expect(identity?.canonicalKey).toBe("github.com/onicode/onicode");
      expect(identity?.locator.remoteUrl).toBe("git@github.com:Onicode/onicode.git");
      expect(identity?.locator.remoteName).toBe("origin");
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect("resolves a remote whose URL contains a space", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-spaced-url-test-",
      });

      yield* git(cwd, ["init"]);
      yield* git(cwd, ["remote", "add", "origin", "ssh://git@git.example.test/team repos/app.git"]);

      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      const identity = yield* resolver.resolve(cwd);
      expect(identity?.locator.remoteUrl).toBe("ssh://git@git.example.test/team repos/app.git");
      expect(identity?.locator.remoteName).toBe("origin");
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect.each(["repo", "repo "] as const)(
    "returns the git top-level root path when resolving from a nested workspace in %j",
    (rootName) =>
      Effect.gen(function* () {
        const fileSystem = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const tempDir = yield* fileSystem.makeTempDirectoryScoped({
          prefix: "oni-repository-identity-nested-root-test-",
        });
        const repoRoot = path.join(tempDir, rootName);
        const nestedWorkspace = path.join(repoRoot, "packages", "web");

        yield* fileSystem.makeDirectory(nestedWorkspace, { recursive: true });
        yield* git(repoRoot, ["init"]);
        yield* git(repoRoot, ["remote", "add", "origin", "git@github.com:Onicode/onicode.git"]);

        const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
        const identity = yield* resolver.resolve(nestedWorkspace);
        const resolvedIdentityRoot =
          identity?.rootPath === undefined ? "" : NodeFS.realpathSync.native(identity.rootPath);
        const resolvedRepoRoot = NodeFS.realpathSync.native(repoRoot);

        expect(identity).not.toBeNull();
        expect(identity?.canonicalKey).toBe("github.com/onicode/onicode");
        expect(normalizeResolvedPath(resolvedIdentityRoot)).toBe(
          normalizeResolvedPath(resolvedRepoRoot),
        );
      }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect.each(["bare clone", "dot-bare"] as const)(
    "resolves a %s to its Git directory and the same identity as a checkout",
    (layout) =>
      Effect.gen(function* () {
        const fileSystem = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const checkout = yield* fileSystem.makeTempDirectoryScoped({
          prefix: "oni-repository-identity-checkout-",
        });
        const root = yield* fileSystem.makeTempDirectoryScoped({
          prefix: "oni-repository-identity-bare-",
        });
        const bareGitDir = layout === "dot-bare" ? path.join(root, ".bare") : root;

        yield* git(checkout, ["init"]);
        yield* git(checkout, ["remote", "add", "origin", "git@github.com:Onicode/onicode.git"]);
        yield* git(root, ["init", "--bare", bareGitDir]);
        if (layout === "dot-bare") {
          yield* fileSystem.writeFileString(path.join(root, ".git"), "gitdir: ./.bare\n");
        }
        yield* git(root, [
          "--git-dir",
          bareGitDir,
          "remote",
          "add",
          "origin",
          "https://github.com/Onicode/onicode.git",
        ]);

        const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
        const checkoutIdentity = yield* resolver.resolve(checkout);
        expect(checkoutIdentity?.canonicalKey).toBe("github.com/onicode/onicode");
        const resolvedBareGitDir = NodeFS.realpathSync.native(bareGitDir);
        const projectRoots = layout === "dot-bare" ? [root, bareGitDir] : [root];

        for (const projectRoot of projectRoots) {
          const identity = yield* resolver.resolve(projectRoot);
          const resolvedIdentityRoot =
            identity?.rootPath === undefined ? "" : NodeFS.realpathSync.native(identity.rootPath);

          expect(identity).not.toBeNull();
          expect(identity?.canonicalKey).toBe(checkoutIdentity?.canonicalKey);
          expect(normalizeResolvedPath(resolvedIdentityRoot)).toBe(
            normalizeResolvedPath(resolvedBareGitDir),
          );
        }

        yield* git(root, ["--git-dir", bareGitDir, "remote", "remove", "origin"]);
        for (const projectRoot of projectRoots) {
          expect(yield* resolver.resolve(projectRoot, { refresh: true })).toBeNull();
        }
      }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect(
    "returns null for non-git, missing and no-remote folders, probing only what can exist",
    () =>
      Effect.gen(function* () {
        const fileSystem = yield* FileSystem.FileSystem;
        const path = yield* Path.Path;
        const nonGitDir = yield* fileSystem.makeTempDirectoryScoped({
          prefix: "oni-repository-identity-non-git-",
        });
        const gitDir = yield* fileSystem.makeTempDirectoryScoped({
          prefix: "oni-repository-identity-no-remote-",
        });

        yield* git(gitDir, ["init"]);

        const runner = yield* ProcessRunner.ProcessRunner;
        const runs: Array<ReadonlyArray<string>> = [];
        const resolver = yield* RepositoryIdentityResolver.make().pipe(
          Effect.provideService(ProcessRunner.ProcessRunner, {
            run: (input) =>
              Effect.suspend(() => {
                runs.push(input.args);
                return runner.run(input);
              }),
          }),
        );
        const bareProbes = () =>
          runs.filter((args) => args.some((arg) => arg.startsWith("--git-dir=")));

        expect(yield* resolver.resolve(path.join(nonGitDir, "deleted"))).toBeNull();
        expect(runs).toEqual([]);

        expect(yield* resolver.resolve(nonGitDir)).toBeNull();
        expect(bareProbes()).toEqual([]);

        expect(yield* resolver.resolve(gitDir)).toBeNull();
      }).pipe(Effect.provide(ProcessRunner.layer)),
  );

  it.effect("ignores local-path remotes such as GitButler's gb-local", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const localOnlyDir = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-local-remote-",
      });
      const mixedDir = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-mixed-remote-",
      });

      yield* git(localOnlyDir, ["init"]);
      yield* git(localOnlyDir, ["remote", "add", "gb-local", "."]);
      yield* git(localOnlyDir, ["remote", "add", "backup", "/srv/git/repo.git"]);
      yield* git(localOnlyDir, ["remote", "add", "archive", "file:///srv/git/repo.git"]);
      yield* git(mixedDir, ["init"]);
      yield* git(mixedDir, ["remote", "add", "gb-local", "."]);
      yield* git(mixedDir, ["remote", "add", "mirror", "git@github.com:Onicode/onicode.git"]);

      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      expect(yield* resolver.resolve(localOnlyDir)).toBeNull();

      const mixedIdentity = yield* resolver.resolve(mixedDir);
      expect(mixedIdentity?.locator.remoteName).toBe("mirror");
      expect(mixedIdentity?.canonicalKey).toBe("github.com/onicode/onicode");
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect.each(["add", "replace"] as const)(
    "refreshes the primary upstream after %s before cache expiry",
    (change) =>
      Effect.gen(function* () {
        const fileSystem = yield* FileSystem.FileSystem;
        const cwd = yield* fileSystem.makeTempDirectoryScoped({
          prefix: "oni-repository-identity-upstream-test-",
        });

        yield* git(cwd, ["init"]);
        yield* git(cwd, ["remote", "add", "origin", "git@github.com:julius/onicode.git"]);
        if (change === "replace") {
          yield* git(cwd, ["remote", "add", "upstream", "git@github.com:Onicode/previous.git"]);
        }

        const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
        const initialIdentity = yield* resolver.resolve(cwd);
        expect(initialIdentity?.canonicalKey).toBe(
          change === "add" ? "github.com/julius/onicode" : "github.com/onicode/previous",
        );

        yield* git(cwd, [
          "remote",
          change === "add" ? "add" : "set-url",
          "upstream",
          "git@github.com:Onicode/onicode.git",
        ]);
        expect(yield* resolver.resolve(cwd)).toEqual(initialIdentity);
        const identity = yield* resolver.resolve(cwd, { refresh: true });

        expect(identity).not.toBeNull();
        expect(identity?.locator.remoteName).toBe("upstream");
        expect(identity?.canonicalKey).toBe("github.com/onicode/onicode");
        expect(identity?.displayName).toBe("Onicode/onicode");
        expect(yield* resolver.resolve(cwd)).toEqual(identity);
      }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect("reports a fork's own remote as origin next to the upstream identity", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-fork-test-",
      });

      yield* git(cwd, ["init"]);
      yield* git(cwd, ["remote", "add", "origin", "git@github.com:julius/onicode-fork.git"]);
      yield* git(cwd, ["remote", "add", "upstream", "git@github.com:Onicode/onicode.git"]);

      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      const identity = yield* resolver.resolve(cwd);

      expect(identity?.canonicalKey).toBe("github.com/onicode/onicode");
      expect(identity?.displayName).toBe("Onicode/onicode");
      expect(identity?.origin).toEqual({
        canonicalKey: "github.com/julius/onicode-fork",
        remoteUrl: "git@github.com:julius/onicode-fork.git",
        displayName: "julius/onicode-fork",
      });
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect("uses the fork as the identity when gh repo set-default points at it", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-gh-default-test-",
      });

      yield* git(cwd, ["init"]);
      yield* git(cwd, ["remote", "add", "origin", "git@github.com:julius/onicode-fork.git"]);
      yield* git(cwd, ["remote", "add", "upstream", "git@github.com:Onicode/onicode.git"]);
      yield* git(cwd, ["config", "remote.origin.gh-resolved", "base"]);

      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      const identity = yield* resolver.resolve(cwd);

      expect(identity?.canonicalKey).toBe("github.com/julius/onicode-fork");
      expect(identity?.locator.remoteName).toBe("origin");
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect("follows an OWNER/REPO mark only to a remote that fetches that repository", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-gh-parent-test-",
      });
      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      const resolveFresh = () => resolver.resolve(cwd, { refresh: true });

      yield* git(cwd, ["init"]);
      yield* git(cwd, ["remote", "add", "fork", "https://github.com/julius/onicode-fork.git"]);
      yield* git(cwd, ["remote", "add", "main", "https://github.com/Onicode/onicode.git"]);
      yield* git(cwd, ["remote", "add", "upstream", "https://github.com/other/onicode.git"]);
      yield* git(cwd, ["config", "remote.fork.gh-resolved", "onicode/onicode"]);
      const parent = yield* resolveFresh();
      expect(parent?.canonicalKey).toBe("github.com/onicode/onicode");
      expect(parent?.locator).toEqual({
        source: "git-remote",
        remoteName: "main",
        remoteUrl: "https://github.com/Onicode/onicode.git",
      });

      // No remote fetches the marked repository, and several marks leave gh's choice unknown.
      yield* git(cwd, ["config", "remote.fork.gh-resolved", "someone/else"]);
      expect((yield* resolveFresh())?.locator.remoteName).toBe("upstream");
      yield* git(cwd, ["config", "remote.fork.gh-resolved", "base"]);
      yield* git(cwd, ["config", "remote.main.gh-resolved", "base"]);
      expect((yield* resolveFresh())?.locator.remoteName).toBe("upstream");

      // The same path on another HTTPS port is a different server.
      yield* git(cwd, ["config", "--unset", "remote.fork.gh-resolved"]);
      yield* git(cwd, ["config", "--unset", "remote.main.gh-resolved"]);
      yield* git(cwd, ["remote", "add", "backup", "https://ghe.example.com:9443/acme/web.git"]);
      yield* git(cwd, ["remote", "add", "ghe-fork", "https://ghe.example.com:8443/me/web.git"]);
      yield* git(cwd, ["remote", "add", "ghe-parent", "https://ghe.example.com:8443/acme/web.git"]);
      yield* git(cwd, ["config", "remote.ghe-fork.gh-resolved", "acme/web"]);
      expect((yield* resolveFresh())?.locator.remoteName).toBe("ghe-parent");
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect("reads the project's gh default when the server inherited GIT_DIR", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const path = yield* Path.Path;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-gh-default-git-dir-",
      });
      const other = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-inherited-git-dir-",
      });

      yield* git(cwd, ["init"]);
      yield* git(cwd, ["remote", "add", "origin", "git@github.com:julius/onicode-fork.git"]);
      yield* git(cwd, ["remote", "add", "upstream", "git@github.com:Onicode/onicode.git"]);
      yield* git(cwd, ["config", "remote.origin.gh-resolved", "base"]);
      yield* git(other, ["init"]);
      yield* git(other, ["config", "remote.upstream.gh-resolved", "base"]);

      vi.stubEnv("GIT_DIR", path.join(other, ".git"));
      yield* Effect.addFinalizer(() => Effect.sync(() => vi.unstubAllEnvs()));

      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      const identity = yield* resolver.resolve(cwd);

      expect(identity?.canonicalKey).toBe("github.com/julius/onicode-fork");
      expect(identity?.locator.remoteName).toBe("origin");
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );

  it.effect("uses the last remote path segment as the repository name for nested groups", () =>
    Effect.gen(function* () {
      const fileSystem = yield* FileSystem.FileSystem;
      const cwd = yield* fileSystem.makeTempDirectoryScoped({
        prefix: "oni-repository-identity-nested-group-test-",
      });

      yield* git(cwd, ["init"]);
      yield* git(cwd, ["remote", "add", "origin", "git@gitlab.com:Onicode/platform/onicode.git"]);

      const resolver = yield* RepositoryIdentityResolver.RepositoryIdentityResolver;
      const identity = yield* resolver.resolve(cwd);

      expect(identity).not.toBeNull();
      expect(identity?.canonicalKey).toBe("gitlab.com/onicode/platform/onicode");
      expect(identity?.displayName).toBe("Onicode/platform/onicode");
      expect(identity?.owner).toBe("Onicode");
      expect(identity?.name).toBe("onicode");
    }).pipe(Effect.provide(RepositoryIdentityResolver.layer)),
  );
});
