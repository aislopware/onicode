// @effect-diagnostics nodeBuiltinImport:off
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
  [/(?<=[a-z])T3(?=[A-Z]|\b)/g, "Oni"],
  [/\bT3(?=[A-Z][a-z])/g, "Oni"],
  [/\bT3(?=[A-Z])/g, "ONI"],
  [/\bt3(?=[A-Z])/g, "oni"],
  [/\bt3(?=[a-z])/g, "oni"],
  [/\bt3_/g, "oni_"],
  [/\bt3-/g, "oni-"],
  [/\bt3\b/g, "onicode"],
  [/\bT3\b/g, "Onicode"],
];
const REPO_RULES: ReadonlyArray<readonly [RegExp, string]> = [
  [
    /https:\/\/t3\.codes\/install\.(sh|ps1)/g,
    "https://raw.githubusercontent.com/aislopware/onicode/main/scripts/install.$1",
  ],
  [/https:\/\/t3\.codes\/download/g, "https://github.com/aislopware/onicode/releases/latest"],
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

export const translateText = (text: string) => applyRules(rebrand(text), REPO_RULES);
export const translatePath = (path: string) => path.split("/").map(rebrand).join("/");

const git = (args: ReadonlyArray<string>, input?: Buffer) =>
  NodeChildProcess.execFileSync("git", args, {
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
const isBinary = (data: Buffer) => data.subarray(0, 8192).includes(0);
const translateBlob = (data: Buffer) =>
  isBinary(data) ? data : Buffer.from(translateText(data.toString("utf8")), "utf8");

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

const pending = () => {
  const base = gitText(["merge-base", "HEAD", UPSTREAM]);
  const done = new Set(readLedger().map(([sha]) => sha));
  const commits = gitText(["rev-list", "--reverse", "--no-merges", `${base}..${UPSTREAM}`])
    .split("\n")
    .filter(Boolean)
    .filter((sha) => ![...done].some((entry) => sha.startsWith(entry)));
  for (const sha of commits) {
    const subject = gitText(["log", "-1", "--format=%s", sha]);
    const paths = gitText(["diff-tree", "-r", "--name-only", "--no-commit-id", sha])
      .split("\n")
      .filter(Boolean);
    const removed = paths.filter((path) => REMOVED_AREAS.test(path));
    const hint =
      removed.length === 0
        ? ""
        : removed.length === paths.length
          ? "  [only touches areas Onicode removed]"
          : `  [${removed.length}/${paths.length} paths in removed areas]`;
    console.log(`${sha.slice(0, 10)}  ${subject}${hint}`);
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

const port = (sha: string) => {
  if (gitText(["status", "--porcelain", "--untracked-files=no"]) !== "") {
    throw new Error("Commit or stash your changes first: a port must land on a clean tree.");
  }
  const full = gitText(["rev-parse", sha]);
  const fields = git(["diff-tree", "-r", "-M", "-z", "--no-commit-id", "--name-status", full])
    .toString("utf8")
    .split("\0")
    .filter(Boolean);
  const report: Array<string> = [];
  let problems = 0;
  const write = (path: string, data: Buffer) => {
    const absolute = NodePath.join(repoRoot, path);
    NodeFS.mkdirSync(NodePath.dirname(absolute), { recursive: true });
    NodeFS.writeFileSync(absolute, data);
    // Intent-to-add, so new files show in `git diff` and `commit -a` without staging conflicts.
    git(["add", "-N", "--", path]);
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
    const base = before === undefined ? Buffer.alloc(0) : translateBlob(before);
    const theirs = after === undefined ? undefined : translateBlob(after);

    if (theirs === undefined) {
      if (ours === undefined) report.push(`gone      ${ourFrom} (already absent here)`);
      else if (ours.equals(base)) {
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
    if (isBinary(theirs) || (ours !== undefined && isBinary(ours))) {
      if (ours === undefined || ours.equals(base)) {
        write(ourTo, theirs);
        report.push(`binary    ${ourTo}`);
      } else {
        problems++;
        report.push(`CONFLICT  ${ourTo}: binary changed on both sides.`);
      }
    } else {
      const { merged, conflicts } = mergeFile(ours ?? Buffer.alloc(0), base, theirs);
      write(ourTo, merged);
      if (conflicts > 0) problems++;
      report.push(
        `${conflicts > 0 ? `CONFLICT  ${ourTo} (${conflicts} hunks)` : `merged    ${ourTo}`}`,
      );
    }
    if (ourTo !== ourFrom && ours !== undefined) git(["rm", "-q", "--cached", "--", ourFrom]);
    if (ourTo !== ourFrom && NodeFS.existsSync(NodePath.join(repoRoot, ourFrom))) {
      NodeFS.rmSync(NodePath.join(repoRoot, ourFrom));
    }
  }
  console.log(report.join("\n"));
  const subject = gitText(["log", "-1", "--format=%s", full]);
  console.log(
    `\n${problems === 0 ? "Applied cleanly." : `${problems} file(s) need a decision.`} Review \`git diff\` line by line, run the tests beside it, then:\n` +
      `  git commit -am ${JSON.stringify(translateText(subject))} -m "Ported from pingdotgg/t3code@${full.slice(0, 10)}."\n` +
      `  node scripts/upstream-sync.ts record ${full.slice(0, 10)} take "<onicode commit or note>"`,
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
