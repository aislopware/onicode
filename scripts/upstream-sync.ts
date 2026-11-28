// @effect-diagnostics nodeBuiltinImport:off globalConsole:off
// Ports t3code (the `upstream` remote) into Onicode one reviewed commit at a time.
// See docs/operations/upstream-sync.md. Never `git merge upstream/main`: the rebrand makes
// a blind merge resurrect removed T3 features and silently undo Onicode changes.
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";

const repoRoot = NodePath.resolve(NodePath.dirname(NodeURL.fileURLToPath(import.meta.url)), "..");
const ledgerPath = NodePath.join(repoRoot, "scripts/upstream-sync.ledger.tsv");
const UPSTREAM = "upstream/main";

// The rebrand's own rules, so upstream text lines up with ours. Protected spans stay verbatim.
const PROTECT =
  /[Tt]3[ _-]?[Cc]onnect|T3CONNECT|T3_CONNECT|[Tt]3[ _.-]?[Cc]hat|T3CHAT|T3_CHAT|pingdotgg\/t3code|-t3\.\d+|t3\.codes|T3 Tools Inc\.?/g;
const RULES: ReadonlyArray<readonly [RegExp, string]> = [
  [/(?<![\w.])com\.t3tools\.t3code/g, "dev.aislopware.onicode"],
  [/(?<![\w.])com\/t3tools\/t3code/g, "dev/aislopware/onicode"],
  [/(?<![\w.])com\.t3tools\.T3Code/g, "dev.onicode.Onicode"],
  [/(?<![\w.])com\.t3tools\./g, "dev.onicode."],
  [/(?<![\w.])com\/t3tools\//g, "dev/onicode/"],
  [/@t3tools\//g, "@onicode/"],
  [/T3CODE_/g, "ONICODE_"],
  [/\bT3_(?=[A-Z0-9])/g, "ONICODE_"],
  [/T3[ -]Code/g, "Onicode"],
  [/t3-code/g, "onicode"],
  [/T3Code/g, "Onicode"],
  [/t3code/g, "onicode"],
  [/T3CODE/g, "ONICODE"],
  [/t3Code/g, "onicode"],
  [/T3Tools/g, "Onicode"],
  [/t3tools/g, "onicode"],
  [/(?<=_)t3_code(?=_)/g, "onicode"],
  [/(?<=_)T3(?=[A-Z_])/g, "ONI"],
  [/(?<=_)t3(?=[A-Za-z_/-])/g, "oni"],
  [/(?<=_)t3\b/g, "onicode"],
  [/(?<=[a-z])T3(?=[A-Z]|\b)/g, "Oni"],
  [/\bT3(?=[A-Z][a-z])/g, "Oni"],
  [/\bT3(?=[A-Z])/g, "ONI"],
  [/\bt3(?=[A-Z])/g, "oni"],
  [/(?<=\\[nrt])t3(?=[A-Z])/g, "oni"],
  [/(?<!\\)\bt3(?=[a-z])/g, "oni"],
  [/(?<!\\)\bt3_/g, "oni_"],
  [/(?<!\\)\bt3-/g, "oni-"],
  // A backslash before it makes the t an escape, as in `\t3`.
  [/(?<!\\)\bt3\b/g, "onicode"],
  [/\bT3\b/g, "Onicode"],
];
const REPO_RULES: ReadonlyArray<readonly [RegExp, string]> = [
  [
    /https:\/\/t3\.codes\/install\.(sh|ps1)/g,
    "https://raw.githubusercontent.com/aislopware/onicode/main/scripts/install.$1",
  ],
  [
    /https:\/\/t3\.codes\/download/g,
    "https://github.com/aislopware/onicode/blob/main/docs/user/install.md",
  ],
  [/pingdotgg\/t3code/g, "aislopware/onicode"],
  [/PingDotGG\/(?:T3Code|T3code|Onicode)/g, "AislopWare/OniCode"],
  [/codething-mvp/g, "sample-app"],
  [/\bpingdotgg\b/g, "aislopware"],
  [/([A-Za-z0-9._-]+)@t3\.codes/g, "$1@onicode.dev"],
  [/https:\/\/t3\.codes(?![A-Za-z0-9./-])/g, "https://github.com/aislopware/onicode"],
];

const applyRules = (text: string, rules: ReadonlyArray<readonly [RegExp, string]>) =>
  rules.reduce((value, [pattern, replacement]) => value.replace(pattern, replacement), text);

const rebrand = (text: string) => {
  let out = "";
  let last = 0;
  for (const match of text.matchAll(PROTECT)) {
    out += applyRules(text.slice(last, match.index), RULES) + match[0];
    last = match.index + match[0].length;
  }
  return out + applyRules(text.slice(last), RULES);
};

const translateAll = (text: string) => applyRules(rebrand(text), REPO_RULES);
// Upstream issue, PR and commit references keep pointing upstream, except in tests, whose
// fixtures pair them with repository names and queries the rules translate.
const UPSTREAM_REF =
  /(pingdotgg\/t3code)(?=#\d|\/(?:(?:pull|issues|discussions)\/\d|commit\/[0-9a-f]))/i;
const isTestFile = (path: string) => /\.test\.[^/]+$/.test(path);

export const translateText = (text: string, path = "") =>
  isTestFile(path)
    ? translateAll(text)
    : text
        .split(UPSTREAM_REF)
        .map((part, index) => (index % 2 === 1 ? part : translateAll(part)))
        .join("");
export const translatePath = (path: string) => path.split("/").map(rebrand).join("/");

// Paths are literal: a name like `src/[id].tsx` must not match `src/i.tsx`.
const git = (args: ReadonlyArray<string>, input?: Buffer) =>
  NodeChildProcess.execFileSync("git", ["--literal-pathspecs", ...args], {
    cwd: repoRoot,
    input,
    maxBuffer: 256 * 1024 * 1024,
  });
const gitText = (args: ReadonlyArray<string>) => git(args).toString("utf8").trim();
const blobAt = (rev: string, path: string): Buffer | undefined => {
  const result = NodeChildProcess.spawnSync("git", ["show", `${rev}:${path}`], {
    cwd: repoRoot,
    maxBuffer: 256 * 1024 * 1024,
  });
  return result.status === 0 ? result.stdout : undefined;
};
const modeAt = (rev: string, path: string) =>
  git(["ls-tree", "-z", rev, "--", path]).toString("utf8").split(" ")[0] || undefined;

/** Writes a blob as git checks it out: a symlink (120000), or a file with its executable bit. */
export const writeBlob = (absolute: string, data: Buffer, mode: string | undefined) => {
  NodeFS.mkdirSync(NodePath.dirname(absolute), { recursive: true });
  // Writing to an existing link would change its target instead.
  if (
    mode === "120000" ||
    NodeFS.lstatSync(absolute, { throwIfNoEntry: false })?.isSymbolicLink()
  ) {
    NodeFS.rmSync(absolute, { force: true });
  }
  if (mode === "120000") {
    NodeFS.symlinkSync(data.toString("utf8"), absolute);
    return;
  }
  NodeFS.writeFileSync(absolute, data);
  if (mode !== undefined) NodeFS.chmodSync(absolute, mode === "100755" ? 0o755 : 0o644);
};
const isBinary = (data: Buffer) => data.subarray(0, 8192).includes(0);
const translateBlob = (data: Buffer, path: string) =>
  isBinary(data) ? data : Buffer.from(translateText(data.toString("utf8"), path), "utf8");
// A symlink's blob is a path, so it is renamed the way paths are, not rewritten as text.
const translateBlobAt = (data: Buffer, path: string, mode: string | undefined) =>
  mode === "120000"
    ? Buffer.from(translatePath(data.toString("utf8")), "utf8")
    : translateBlob(data, path);

const readLedger = () =>
  NodeFS.existsSync(ledgerPath)
    ? NodeFS.readFileSync(ledgerPath, "utf8")
        .split("\n")
        .slice(1)
        .filter(Boolean)
        .map((line) => line.split("\t"))
    : [];

const REMOVED_AREAS =
  /^(infra|apps\/marketing)\/|(^|\/)(cloud|relay|clerk|telemetry|analytics)(\/|[A-Z.]|$)|[Cc]onnect|vercel|nightly|\.github\/workflows\/(release|desktop-macos-preview|mobile-android-apk|web-preview)/;

// Where Onicode diverges from upstream or ships to clients it cannot update in place.
// A port touching these gets its checks per commit (docs/operations/upstream-sync.md).
const RISK_AREAS: ReadonlyArray<readonly [string, RegExp]> = [
  ["contracts", /^packages\/contracts\//],
  ["migrations", /\/[Mm]igrations?\//],
  ["sidebar", /^apps\/web\/src\/(components\/Sidebar\.tsx|state\/olderSettledShelf\.ts)$/],
  ["mobile", /^apps\/mobile\//],
  ["desktop", /^apps\/desktop\/|^apps\/server\/src\/desktopUpdate\//],
  ["auth", /^apps\/server\/src\/(auth|peer)\//],
  ["provider", /^apps\/server\/src\/provider\//],
  ["ci", /^\.github\//],
  ["deps", /(^|\/)package\.json$/],
];
const LARGE_CHANGE_LINES = 400;

export const riskAreas = (paths: ReadonlyArray<string>, changedLines: number) => [
  ...RISK_AREAS.filter(([, pattern]) => paths.some((path) => pattern.test(path))).map(
    ([name]) => name,
  ),
  ...(changedLines > LARGE_CHANGE_LINES ? [`${changedLines} lines`] : []),
];

const pending = () => {
  const base = gitText(["merge-base", "HEAD", UPSTREAM]);
  const done = new Set(readLedger().flatMap(([sha]) => (sha ? [sha] : [])));
  const commits = gitText(["rev-list", "--reverse", "--no-merges", `${base}..${UPSTREAM}`])
    .split("\n")
    .filter(Boolean)
    .filter((sha) => ![...done].some((entry) => sha.startsWith(entry)));
  for (const sha of commits) {
    const subject = gitText(["log", "-1", "--format=%s", sha]);
    const stats = gitText(["diff-tree", "-r", "--numstat", "--no-commit-id", sha])
      .split("\n")
      .filter(Boolean)
      .map((line) => line.split("\t"));
    const paths = stats.map(([, , path]) => path ?? "");
    const changedLines = stats.reduce(
      (total, [added, removed]) => total + (Number(added) || 0) + (Number(removed) || 0),
      0,
    );
    const removed = paths.filter((path) => REMOVED_AREAS.test(path));
    const hint =
      removed.length === 0
        ? ""
        : removed.length === paths.length
          ? "  [only touches areas Onicode removed]"
          : `  [${removed.length}/${paths.length} paths in removed areas]`;
    const risk = riskAreas(paths, changedLines);
    const riskHint = risk.length === 0 ? "" : `  [risky: ${risk.join(", ")}]`;
    console.log(`${sha.slice(0, 10)}  ${subject}${hint}${riskHint}`);
  }
  if (commits.length === 0) console.log("Nothing pending.");
};

const mergeFile = (ours: Buffer, base: Buffer, theirs: Buffer) => {
  const dir = NodeFS.mkdtempSync(NodePath.join(NodeOS.tmpdir(), "upstream-sync-"));
  try {
    const files = ["ours", "base", "theirs"].map((name) => NodePath.join(dir, name));
    [ours, base, theirs].forEach((data, index) => NodeFS.writeFileSync(files[index]!, data));
    const result = NodeChildProcess.spawnSync(
      "git",
      ["merge-file", "-p", "-L", "onicode", "-L", "upstream-before", "-L", "upstream", ...files],
      { maxBuffer: 256 * 1024 * 1024 },
    );
    if (result.status === null || result.status < 0) throw new Error("git merge-file failed");
    return { merged: result.stdout, conflicts: result.status };
  } finally {
    NodeFS.rmSync(dir, { recursive: true, force: true });
  }
};

const shellQuote = (value: string) => `'${value.replaceAll("'", `'\\''`)}'`;

/** The port's commit keeps the upstream author and co-authors, as `git cherry-pick` would. */
export const portCommitCommand = (input: {
  readonly sha: string;
  readonly subject: string;
  readonly author: string;
  readonly coAuthors: ReadonlyArray<string>;
}) =>
  [
    "git commit -a",
    `--author=${shellQuote(input.author)}`,
    `-m ${shellQuote(translateText(input.subject))}`,
    `-m ${shellQuote(`Ported from pingdotgg/t3code@${input.sha.slice(0, 10)}.`)}`,
    ...[...new Set(input.coAuthors.map((name) => name.trim()))]
      .filter((name) => name !== "" && name !== input.author)
      .map((name) => `--trailer ${shellQuote(`Co-authored-by: ${name}`)}`),
  ].join(" ");

const port = (sha: string) => {
  if (gitText(["status", "--porcelain", "--untracked-files=no"]) !== "") {
    throw new Error("Commit or stash your changes first: a port must land on a clean tree.");
  }
  const full = gitText(["rev-parse", sha]);
  // Git for Windows sets this where links are unsupported.
  const symlinks = NodeChildProcess.spawnSync("git", ["config", "--bool", "core.symlinks"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  if (
    symlinks.stdout.trim() === "false" &&
    gitText(["diff-tree", "-r", "--no-commit-id", full])
      .split("\n")
      .some((line) => line.split(" ")[1] === "120000")
  ) {
    throw new Error("Port on a checkout with core.symlinks: upstream's links must land as links.");
  }
  const fields = git(["diff-tree", "-r", "-M", "-z", "--no-commit-id", "--name-status", full])
    .toString("utf8")
    .split("\0")
    .filter(Boolean);
  const report: Array<string> = [];
  let problems = 0;
  const write = (path: string, data: Buffer, mode: string | undefined) => {
    writeBlob(NodePath.join(repoRoot, path), data, mode);
    // Intent-to-add, so new files show in `git diff` and `commit -a` without staging conflicts.
    git(["add", "-N", "--", path]);
    // Stages only the bit, which core.fileMode=false hides from git; `--chmod` would stage content.
    const [indexMode, oid] = gitText(["ls-files", "-s", "--", path]).split(" ");
    if (
      (mode === "100755" || mode === "100644") &&
      (indexMode === "100755" || indexMode === "100644") &&
      indexMode !== mode
    ) {
      git(["update-index", "--cacheinfo", `${mode},${oid},${path}`]);
    }
  };
  for (let index = 0; index < fields.length;) {
    const status = fields[index++]!;
    const from = fields[index++]!;
    const to = status.startsWith("R") || status.startsWith("C") ? fields[index++]! : from;
    const ourFrom = translatePath(from);
    const ourTo = translatePath(to);
    const before = blobAt(`${full}^`, from);
    const after = status.startsWith("D") ? undefined : blobAt(full, to);
    const ours = blobAt("HEAD", ourFrom);
    const baseMode = before === undefined ? undefined : modeAt(`${full}^`, from);
    const theirMode = after === undefined ? undefined : modeAt(full, to);
    const ourMode = ours === undefined ? undefined : modeAt("HEAD", ourFrom);
    const base = before === undefined ? Buffer.alloc(0) : translateBlobAt(before, from, baseMode);
    const theirs = after === undefined ? undefined : translateBlobAt(after, to, theirMode);
    const oursUnchanged = ours !== undefined && ours.equals(base) && ourMode === baseMode;

    if (theirs === undefined) {
      if (ours === undefined) report.push(`gone      ${ourFrom} (already absent here)`);
      else if (oursUnchanged) {
        git(["rm", "-q", "--", ourFrom]);
        report.push(`deleted   ${ourFrom}`);
      } else {
        problems++;
        report.push(
          `CONFLICT  ${ourFrom}: upstream deleted it, Onicode changed it. Decide by hand.`,
        );
      }
      continue;
    }
    if (ours === undefined && before !== undefined) {
      problems++;
      report.push(
        `MISSING   ${ourFrom}: upstream changed a file Onicode does not have (removed feature?)`,
      );
      continue;
    }
    if (
      ourMode !== undefined &&
      before !== undefined &&
      ourMode !== baseMode &&
      theirMode !== baseMode &&
      ourMode !== theirMode
    ) {
      problems++;
      report.push(
        `CONFLICT  ${ourTo}: mode ${baseMode} became ${ourMode} here, ${theirMode} upstream.`,
      );
      continue;
    }
    // Upstream's mode where it changed it or added the file, else the one Onicode has.
    const mode =
      before === undefined || theirMode !== baseMode ? theirMode : (ourMode ?? theirMode);
    // A link target or a binary merges as a whole: a text merge would write conflict markers.
    const whole =
      [baseMode, theirMode, ourMode].includes("120000") ||
      isBinary(base) ||
      isBinary(theirs) ||
      (ours !== undefined && isBinary(ours));
    let wrote = true;
    if (whole) {
      const content =
        ours === undefined || ours.equals(base)
          ? theirs
          : theirs.equals(base) || theirs.equals(ours)
            ? ours
            : undefined;
      if (content === undefined) {
        problems++;
        wrote = false;
        report.push(
          `CONFLICT  ${ourTo}: ${mode === "120000" ? "symlink" : "binary"} changed on both sides.`,
        );
      } else if (
        // A type change on one side only: the other side's content cannot take that type.
        ours !== undefined &&
        (mode === "120000") !== ((content === ours ? ourMode : theirMode) === "120000")
      ) {
        problems++;
        wrote = false;
        report.push(`CONFLICT  ${ourTo}: one side made it a symlink, the other changed it.`);
      } else {
        write(ourTo, content, mode);
        report.push(`${mode === "120000" ? "symlink   " : "binary    "}${ourTo}`);
      }
    } else {
      const { merged, conflicts } = mergeFile(ours ?? Buffer.alloc(0), base, theirs);
      write(ourTo, merged, mode);
      if (conflicts > 0) problems++;
      report.push(
        `${conflicts > 0 ? `CONFLICT  ${ourTo} (${conflicts} hunks)` : `merged    ${ourTo}`}`,
      );
    }
    if (wrote && ourMode !== undefined && mode !== ourMode) {
      report.push(`mode      ${ourTo} ${ourMode} -> ${mode}`);
    }
    if (!wrote || ourTo === ourFrom) continue;
    if (ours !== undefined) git(["rm", "-q", "--cached", "--", ourFrom]);
    // lstat, not exists: a dangling link is still there to remove.
    if (NodeFS.lstatSync(NodePath.join(repoRoot, ourFrom), { throwIfNoEntry: false })) {
      NodeFS.rmSync(NodePath.join(repoRoot, ourFrom));
    }
  }
  console.log(report.join("\n"));
  const [subject = "", author = "", ...coAuthors] = gitText([
    "log",
    "-1",
    "--format=%s%n%an <%ae>%n%(trailers:key=Co-authored-by,valueonly)",
    full,
  ]).split("\n");
  console.log(
    `\n${problems === 0 ? "Applied cleanly." : `${problems} file(s) need a decision.`} Review \`git diff HEAD\` line by line, run the tests beside it, then:\n` +
      `  node scripts/upstream-sync.ts record ${full.slice(0, 10)} take "<onicode commit or note>"\n` +
      `  ${portCommitCommand({ sha: full, subject, author, coAuthors })}`,
  );
};

const record = (sha: string, decision: string, note: string) => {
  if (decision !== "take" && decision !== "skip") throw new Error("decision is take or skip");
  if (note.trim() === "") throw new Error("say why (skip) or which Onicode commit (take)");
  const full = gitText(["rev-parse", sha]).slice(0, 10);
  if (!NodeFS.existsSync(ledgerPath))
    NodeFS.writeFileSync(ledgerPath, "upstream\tdecision\tnote\n");
  NodeFS.appendFileSync(ledgerPath, `${full}\t${decision}\t${note.replace(/[\t\n]/g, " ")}\n`);
  console.log(`recorded ${full} ${decision}`);
};

const show = (sha: string) => {
  process.stdout.write(translateText(git(["show", "--stat", "--patch", sha]).toString("utf8")));
};

const [command, ...args] = process.argv.slice(2);
if (import.meta.main) {
  if (command === "pending") pending();
  else if (command === "show" && args[0]) show(args[0]);
  else if (command === "port" && args[0]) port(args[0]);
  else if (command === "record" && args.length >= 3)
    record(args[0]!, args[1]!, args.slice(2).join(" "));
  else {
    console.log(
      "usage: node scripts/upstream-sync.ts pending | show <sha> | port <sha> | record <sha> take|skip <note>",
    );
    process.exitCode = command === undefined ? 0 : 1;
  }
}
