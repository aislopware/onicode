// @effect-diagnostics nodeBuiltinImport:off
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import { describe, expect, it } from "vite-plus/test";

import {
  portCommitCommand,
  riskAreas,
  translatePath,
  translateText,
  writeBlob,
} from "./upstream-sync.ts";

describe("upstream-sync translation", () => {
  it.each([
    ['import { x } from "@t3tools/contracts";', 'import { x } from "@onicode/contracts";'],
    ["process.env.T3CODE_HOME ?? T3_PORT", "process.env.ONICODE_HOME ?? ONICODE_PORT"],
    ["T3 Code keeps a t3code checkpoint", "Onicode keeps a onicode checkpoint"],
    ["new T3ConnectClient()", "new T3ConnectClient()"],
    [
      "see https://github.com/pingdotgg/t3code/pull/42, pingdotgg/t3code#7 and https://github.com/pingdotgg/t3code/commit/1bc8e2d",
      "see https://github.com/pingdotgg/t3code/pull/42, pingdotgg/t3code#7 and https://github.com/pingdotgg/t3code/commit/1bc8e2d",
    ],
    [
      "https://github.com/PingDotGG/T3Code/pull/42 and pingdotgg/t3code/commit/ABC1234",
      "https://github.com/PingDotGG/T3Code/pull/42 and pingdotgg/t3code/commit/ABC1234",
    ],
    [
      "git clone https://github.com/pingdotgg/t3code",
      "git clone https://github.com/aislopware/onicode",
    ],
    [
      "https://github.com/pingdotgg/t3code/issues/new?template=bug.yml",
      "https://github.com/aislopware/onicode/issues/new?template=bug.yml",
    ],
    [
      "curl -fsSL https://t3.codes/install.sh | sh",
      "curl -fsSL https://raw.githubusercontent.com/aislopware/onicode/main/scripts/install.sh | sh",
    ],
    ["react-native-screens-5.0.0-t3.48.tgz", "react-native-screens-5.0.0-t3.48.tgz"],
    // A tab escape before a digit is not the t3 brand.
    ['"Apples green\\t3\\t\\nAfter"', '"Apples green\\t3\\t\\nAfter"'],
    ['"row\\t3A"', '"row\\t3A"'],
    // An escaped backslash before it leaves the brand.
    ['"C:\\\\t3\\\\bin"', '"C:\\\\onicode\\\\bin"'],
    ['"C:\\\\t3-cli"', '"C:\\\\oni-cli"'],
    [
      "runInNewContext(`${source}\\nt3McpExtension(pi)`)",
      "runInNewContext(`${source}\\noniMcpExtension(pi)`)",
    ],
    [
      "PI_T3_MCP_EXTENSION_SOURCE ?? __T3_SETUP_DONE__",
      "PI_ONI_MCP_EXTENSION_SOURCE ?? __ONI_SETUP_DONE__",
    ],
    [
      "globalThis.__t3Recorder ?? msg_t3_turn_ ?? _t3/empty",
      "globalThis.__oniRecorder ?? msg_oni_turn_ ?? _oni/empty",
    ],
    ['first_t3="$(command -v t3)"', 'first_onicode="$(command -v onicode)"'],
    ["mcp__t3_code__delegate_task", "mcp__onicode__delegate_task"],
    // Onicode has no T3 Chat theme, so its names stay upstream's and a port that uses them fails.
    [
      'T3_CHAT_THEME.id === "t3-chat" && host === "t3.chat"',
      'T3_CHAT_THEME.id === "t3-chat" && host === "t3.chat"',
    ],
    ["com.t3tools.t3code.widgets", "dev.aislopware.onicode.widgets"],
    ["java/com/t3tools/t3code/MainActivity.kt", "java/dev/aislopware/onicode/MainActivity.kt"],
  ])("%s", (upstream, onicode) => {
    expect(translateText(upstream)).toBe(onicode);
  });

  it.each([
    "packages/shared/src/paletteSearch.test.ts",
    ".github/scripts/stage-preview-bundle.test.py",
  ])("translates upstream references in %s with the repository name beside them", (path) => {
    expect(
      translateText(
        'repository: "pingdotgg/t3code", url: "https://github.com/pingdotgg/t3code/pull/42"',
        path,
      ),
    ).toBe(
      'repository: "aislopware/onicode", url: "https://github.com/aislopware/onicode/pull/42"',
    );
  });

  it("renames t3 paths the way the rebrand did", () => {
    expect(translatePath("apps/mobile/modules/t3-terminal/ios/T3TerminalView.swift")).toBe(
      "apps/mobile/modules/oni-terminal/ios/OniTerminalView.swift",
    );
    expect(translatePath("packages/shared/src/git.ts")).toBe("packages/shared/src/git.ts");
  });
});

describe("upstream-sync port commit", () => {
  it("keeps the upstream author and co-authors on the Onicode commit", () => {
    const repo = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "upstream-sync-commit-"));
    try {
      const git = (...args: ReadonlyArray<string>) =>
        NodeChildProcess.execFileSync("git", args, { cwd: repo, encoding: "utf8" }).trim();
      git("init", "-q");
      git("config", "user.name", "Onicode Maintainer");
      git("config", "user.email", "maintainer@example.test");
      git("config", "commit.gpgsign", "false");
      git("config", "core.hooksPath", "/dev/null");
      NodeFS.writeFileSync(NodePath.join(repo, "file.txt"), "before\n");
      git("add", "file.txt");
      git("commit", "-qm", "seed");
      NodeFS.writeFileSync(NodePath.join(repo, "file.txt"), "after\n");

      const author = "Julius O'Brien <julius@example.test>";
      NodeChildProcess.execSync(
        portCommitCommand({
          sha: "7e45dcdd61942973b4f026d01ca89b4128d438c8",
          subject: "fix(web): T3 Code's `$HOME` stays put (#1)",
          author,
          coAuthors: ["Claude Opus 5.5 <noreply@anthropic.com>", author, ""],
        }),
        { cwd: repo, stdio: "ignore" },
      );

      expect(git("log", "-1", "--format=%an <%ae>|%cn")).toBe(`${author}|Onicode Maintainer`);
      expect(git("log", "-1", "--format=%B")).toBe(
        "fix(web): Onicode's `$HOME` stays put (#1)\n\n" +
          "Ported from pingdotgg/t3code@7e45dcdd61.\n\n" +
          "Co-authored-by: Claude Opus 5.5 <noreply@anthropic.com>",
      );
    } finally {
      NodeFS.rmSync(repo, { recursive: true, force: true });
    }
  });
});

describe("upstream-sync blob writes", () => {
  it("lands a symlink as a link and keeps an executable's bit", () => {
    const dir = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "upstream-sync-modes-"));
    try {
      const link = NodePath.join(dir, "ios", "Engine.swift");
      NodeFS.mkdirSync(NodePath.dirname(link), { recursive: true });
      NodeFS.writeFileSync(NodePath.join(dir, "target.txt"), "kept\n");
      NodeFS.symlinkSync("../target.txt", link);
      // An existing link is replaced, not written through to its target.
      writeBlob(link, Buffer.from("../shared/Engine.swift"), "120000");
      const script = NodePath.join(dir, "bin", "run.sh");
      writeBlob(script, Buffer.from("#!/bin/sh\n"), "100755");
      const plain = NodePath.join(dir, "bin", "notes.txt");
      writeBlob(plain, Buffer.from("notes\n"), "100644");

      expect([
        NodeFS.readlinkSync(link),
        NodeFS.readFileSync(NodePath.join(dir, "target.txt"), "utf8"),
        NodeFS.statSync(script).mode & 0o111,
        NodeFS.statSync(plain).mode & 0o111,
      ]).toEqual(["../shared/Engine.swift", "kept\n", 0o111, 0]);
    } finally {
      NodeFS.rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe("upstream-sync port modes", () => {
  it("ports links and modes whole, and keeps Onicode's side where both changed one", () => {
    const repo = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "upstream-sync-port-"));
    try {
      const at = (path: string) => NodePath.join(repo, path);
      const git = (...args: ReadonlyArray<string>) =>
        NodeChildProcess.execFileSync("git", args, { cwd: repo, encoding: "utf8" }).trim();
      const file = (path: string, data: string | Buffer, mode = 0o644) => {
        NodeFS.mkdirSync(NodePath.dirname(at(path)), { recursive: true });
        NodeFS.rmSync(at(path), { force: true });
        NodeFS.writeFileSync(at(path), data);
        NodeFS.chmodSync(at(path), mode);
      };
      const link = (path: string, target: string) => {
        NodeFS.mkdirSync(NodePath.dirname(at(path)), { recursive: true });
        NodeFS.rmSync(at(path), { force: true });
        NodeFS.symlinkSync(target, at(path));
      };
      const commit = (message: string) => {
        git("add", "-A", ".");
        git("commit", "-qm", message);
      };
      git("init", "-q", "-b", "main");
      git("config", "user.name", "Onicode Maintainer");
      git("config", "user.email", "maintainer@example.test");
      git("config", "commit.gpgsign", "false");
      git("config", "core.hooksPath", "/dev/null");
      // The exec bit must still reach the index where git ignores the worktree's.
      git("config", "core.fileMode", "false");
      link("both.link", "a.txt");
      file("edited.txt", "base\n");
      link("moved.link", "nowhere");
      file("chmodded.txt", "same\n");
      file("image.bin", Buffer.from([0, 1, 2]));
      file("typed.txt", "plain\n");
      file("notes.txt", "one\n");
      link("retarget.link", "old.txt");
      file("blob.dat", Buffer.from([0, 1]));
      file("foo*.txt", "glob\n");
      file("foo-other.txt", "other\n");
      file("relinked.txt", "dest");
      commit("base");
      git("branch", "upstream");

      git("checkout", "-q", "upstream");
      link("new.link", "../t3code.txt");
      file("run.sh", "#!/bin/sh\n", 0o755);
      git("add", "run.sh");
      git("update-index", "--chmod=+x", "--", "run.sh");
      link("both.link", "b.txt");
      file("edited.txt", "base\nupstream\n");
      git("mv", "moved.link", "renamed.link");
      git("rm", "-q", "chmodded.txt");
      file("notes.txt", "one\ntwo\n");
      link("retarget.link", "same.txt");
      file("blob.dat", "upstream text\n");
      NodeFS.rmSync(at("foo*.txt"));
      link("relinked.txt", "dest");
      git("add", "-A", ".");
      git("update-index", "--chmod=+x", "--", "image.bin", "typed.txt");
      commit("upstream");
      const upstream = git("rev-parse", "HEAD");

      git("checkout", "-q", "main");
      link("both.link", "c.txt");
      link("edited.txt", "elsewhere.txt");
      link("typed.txt", "plain.txt");
      git("add", "-A", ".");
      git("update-index", "--chmod=+x", "--", "chmodded.txt");
      file("image.bin", Buffer.from([0, 9, 9]));
      link("retarget.link", "same.txt");
      file("blob.dat", "onicode text\n");
      file("relinked.txt", "edited");
      git("add", "-A", ".");
      commit("onicode");
      file(
        "scripts/upstream-sync.ts",
        NodeFS.readFileSync(new URL("./upstream-sync.ts", import.meta.url)),
      );

      const port = () =>
        NodeChildProcess.spawnSync(
          process.execPath,
          ["scripts/upstream-sync.ts", "port", upstream],
          {
            cwd: repo,
            encoding: "utf8",
          },
        );
      git("config", "core.symlinks", "false");
      expect(port().stderr).toContain("upstream's links must land as links");
      git("config", "core.symlinks", "true");
      const ported = NodeChildProcess.spawnSync(
        process.execPath,
        ["scripts/upstream-sync.ts", "port", upstream],
        { cwd: repo, encoding: "utf8" },
      );
      expect(ported.stderr).toBe("");
      const modes = Object.fromEntries(
        git("ls-files", "-s")
          .split("\n")
          .map((line) => [line.split("\t")[1], line.split(" ")[0]]),
      );

      expect({
        newLink: NodeFS.readlinkSync(at("new.link")),
        newLinkMode: modes["new.link"],
        script: modes["run.sh"],
        // Both changed the target: Onicode's link stays, and the port says so.
        bothLink: NodeFS.readlinkSync(at("both.link")),
        // Onicode made it a link and upstream edited the file: the link stays.
        edited: NodeFS.readlinkSync(at("edited.txt")),
        // Onicode made it a link, upstream made the file executable.
        typed: NodeFS.readlinkSync(at("typed.txt")),
        // A dangling link moves too.
        renamed: NodeFS.readlinkSync(at("renamed.link")),
        movedGone: NodeFS.lstatSync(at("moved.link"), { throwIfNoEntry: false }),
        // Upstream deleted a file Onicode only made executable: it stays.
        chmodded: modes["chmodded.txt"],
        // Onicode's bytes, with upstream's new exec bit.
        image: [...NodeFS.readFileSync(at("image.bin"))],
        imageMode: modes["image.bin"],
        // Both sides made the same change.
        retarget: NodeFS.readlinkSync(at("retarget.link")),
        // A binary base both sides replaced with text: Onicode's text stays.
        blob: NodeFS.readFileSync(at("blob.dat"), "utf8"),
        // A path with a glob character deletes that file only.
        globGone: NodeFS.existsSync(at("foo*.txt")),
        other: NodeFS.existsSync(at("foo-other.txt")),
        // Upstream made it a link to its old bytes, Onicode edited the file.
        relinked: NodeFS.readFileSync(at("relinked.txt"), "utf8"),
        // Merged content stays unstaged for review; only the bit is staged.
        unstaged: git("diff", "--name-only").split("\n"),
        staged: git("diff", "--cached", "--name-only").split("\n"),
      }).toEqual({
        newLink: "../onicode.txt",
        newLinkMode: "120000",
        script: "100755",
        bothLink: "c.txt",
        edited: "elsewhere.txt",
        typed: "plain.txt",
        renamed: "nowhere",
        movedGone: undefined,
        chmodded: "100755",
        image: [0, 9, 9],
        imageMode: "100755",
        retarget: "same.txt",
        blob: "onicode text\n",
        globGone: false,
        other: true,
        relinked: "edited",
        unstaged: expect.arrayContaining(["notes.txt", "run.sh"]),
        staged: expect.not.arrayContaining(["notes.txt"]),
      });
      expect(ported.stdout).toContain("CONFLICT  blob.dat: binary changed on both sides.");
      expect(ported.stdout).toContain("CONFLICT  both.link: symlink changed on both sides.");
      expect(ported.stdout).toContain("CONFLICT  edited.txt: symlink changed on both sides.");
      expect(ported.stdout).toContain("CONFLICT  relinked.txt: one side made it a symlink");
      expect(ported.stdout).toContain(
        "CONFLICT  typed.txt: mode 100644 became 120000 here, 100755",
      );
      expect(ported.stdout).toContain("CONFLICT  chmodded.txt: upstream deleted it");
    } finally {
      NodeFS.rmSync(repo, { recursive: true, force: true });
    }
  });
});

describe("upstream-sync risk areas", () => {
  it.each([
    [["apps/server/src/git/GitCore.ts"], 40, []],
    [
      ["packages/contracts/src/orchestration.ts", "apps/mobile/src/App.tsx"],
      40,
      ["contracts", "mobile"],
    ],
    [["apps/server/src/persistence/Migrations/082_Example.ts"], 40, ["migrations"]],
    [["apps/web/src/components/Sidebar.tsx"], 40, ["sidebar"]],
    [["apps/web/src/components/SidebarRow.tsx"], 40, []],
    [
      ["apps/server/src/desktopUpdate/Service.ts", "apps/server/src/peer/Link.ts"],
      40,
      ["desktop", "auth"],
    ],
    [["apps/server/package.json", ".github/workflows/ci.yml"], 40, ["ci", "deps"]],
    [["apps/web/src/composer.ts"], 401, ["401 lines"]],
  ])("%j with %i changed lines", (paths, changedLines, expected) => {
    expect(riskAreas(paths, changedLines)).toEqual(expected);
  });
});
