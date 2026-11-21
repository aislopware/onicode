import { describe, expect, it } from "vite-plus/test";

import { planClaudeSkillDispatch, type ClaudeSkillTarget } from "./ClaudeSkillDispatch.ts";

const skillPath = (name: string) => `/home/me/.claude/skills/${name}/SKILL.md`;

const skillMap = (names: Iterable<string>) =>
  new Map<string, ClaudeSkillTarget>([...names].map((name) => [name, { path: skillPath(name) }]));

const SKILLS = skillMap(["2spec", "implement", "review", "re-release-version"]);

describe("planClaudeSkillDispatch", () => {
  // regression: #9128 skills picked from the composer did not run
  it("leaves a prompt without a known skill untouched", () => {
    expect(planClaudeSkillDispatch("fix the build", SKILLS)).toBeUndefined();
    expect(planClaudeSkillDispatch("echo $HOME then $unknown", SKILLS)).toBeUndefined();
  });

  it("keeps the whole request ahead of a mid-prompt slash command", () => {
    expect(planClaudeSkillDispatch("ok, now $implement all the tickets", SKILLS)).toEqual({
      leadingText: "ok, now /implement all the tickets",
      commandText: "/implement all the tickets",
    });
  });

  it("keeps a mention that opens the prompt as a single command block", () => {
    expect(planClaudeSkillDispatch("$review\nfocus on auth", SKILLS)).toEqual({
      leadingText: undefined,
      commandText: "/review\nfocus on auth",
    });
  });

  it("dispatches the last mention and rewrites earlier ones inline", () => {
    expect(planClaudeSkillDispatch("$review the diff, then $implement the fixes", SKILLS)).toEqual({
      leadingText: "/review the diff, then /implement the fixes",
      commandText: "/implement the fixes",
    });
    const windows = new Map<string, ClaudeSkillTarget>([
      [
        "zipped:plan",
        {
          path: "C:\\cache.zip\\plugin.zip\\skills\\plan\\SKILL.md",
          archive: "C:\\cache.zip\\plugin.zip",
          userInvocationOnly: true,
          instructions: "Write the plan.",
        },
      ],
      ["implement", { path: skillPath("implement") }],
    ]);
    expect(planClaudeSkillDispatch("$zipped:plan then $implement", windows)?.leadingText).toContain(
      "Its other files are in skills/plan/ inside the archive C:\\cache.zip\\plugin.zip.]",
    );
  });

  it("dispatches currency-prefixed mentions and preserves their source boundaries", () => {
    for (const symbol of ["€", "£", "¥", "₹", "₩", "₿", "𑿝"]) {
      expect(
        planClaudeSkillDispatch(
          `${symbol}review the diff, then ${symbol}implement the fixes`,
          SKILLS,
        ),
      ).toEqual({
        leadingText: "/review the diff, then /implement the fixes",
        commandText: "/implement the fixes",
      });
      expect(planClaudeSkillDispatch(`${symbol}2spec for this`, SKILLS)).toEqual({
        leadingText: undefined,
        commandText: "/2spec for this",
      });
      expect(planClaudeSkillDispatch(`5${symbol}review ${symbol}unknown`, SKILLS)).toBeUndefined();
    }
  });

  it("ignores currency amounts and compact monetary expressions", () => {
    const skillsWithCurrency = skillMap([...SKILLS.keys(), "20", "20k", "100M", "1e6"]);
    for (const symbol of ["$", "€", "£", "¥", "₹", "₩", "₿", "𑿝"]) {
      expect(
        planClaudeSkillDispatch(
          `pay ${symbol}20 ${symbol}20k ${symbol}100M ${symbol}1e6 tomorrow`,
          skillsWithCurrency,
        ),
      ).toBeUndefined();
    }
  });

  it("caps inlined skill instructions and adds no note for an empty one", () => {
    const inline = (instructions: string) => ({
      path: "/plugins/skills.zip/SKILL.md",
      archive: "/plugins/skills.zip",
      userInvocationOnly: true,
      instructions,
    });
    const skills = new Map<string, ClaudeSkillTarget>([
      // The cut would split the emoji's surrogate pair, so the half goes too.
      ["zipped:big", inline(`${"x".repeat(64 * 1024 - 1)}😀`)],
      ["zipped:more", inline("More")],
      ["zipped:empty", inline("---\nname: empty\n---\n")],
      ["implement", { path: skillPath("implement"), userInvocationOnly: true }],
    ]);
    const plan = planClaudeSkillDispatch(
      "$zipped:big $zipped:more $zipped:empty $implement",
      skills,
    );
    expect(plan?.leadingText).toBe(
      [
        `/zipped:big [The user invoked this skill. Follow its instructions:\n<skill name="zipped:big">\n${"x".repeat(64 * 1024 - 1)}\n[Truncated]\n</skill>\nIts other files are inside the archive /plugins/skills.zip.]`,
        "/zipped:more [The user invoked this skill; its instructions did not fit.]",
        "/zipped:empty",
        "/implement",
      ].join(" "),
    );
  });

  it("bounds notes for repeated user-only skills and leaves the native dispatch unannotated", () => {
    const skills = new Map<string, ClaudeSkillTarget>([
      ["interrogate", { path: skillPath("interrogate"), userInvocationOnly: true }],
      [
        "zipped:plan",
        {
          path: "/home/me/plugin.zip/skills/plan/SKILL.md",
          archive: "/home/me/plugin.zip",
          userInvocationOnly: true,
          instructions: "---\nuser-invocable-only: true\n---\n# Plan\nWrite the plan.\n",
        },
      ],
      ["implement", { path: skillPath("implement"), userInvocationOnly: true }],
    ]);
    expect(
      planClaudeSkillDispatch(
        "$interrogate then $interrogate then $zipped:plan then $implement and $implement the fixes",
        skills,
      ),
    ).toEqual({
      leadingText: `/interrogate [The user invoked this skill. Follow the instructions at: ${skillPath("interrogate")}] then /interrogate then /zipped:plan [The user invoked this skill. Follow its instructions:\n<skill name="zipped:plan">\n# Plan\nWrite the plan.\n</skill>\nIts other files are in skills/plan/ inside the archive /home/me/plugin.zip.] then /implement and /implement the fixes`,
      commandText: "/implement the fixes",
    });
  });
});
