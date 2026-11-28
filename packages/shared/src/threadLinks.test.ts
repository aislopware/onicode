import { describe, expect, it, vi } from "vite-plus/test";
import { EnvironmentId, ThreadId, type ScopedThreadRef } from "@onicode/contracts";
import { mFallback } from "gt-i18n";
import type { MFunctionType } from "gt-i18n/types";
import remarkParse from "remark-parse";
import { unified } from "unified";

import {
  formatThreadLink,
  guessThreadLinkRef,
  parseThreadLinkHref,
  replaceThreadLinks,
  threadLinkCandidates,
  threadLinkRefs,
} from "./threadLinks.ts";
import { relabelThreadLinks } from "./threadLinksCopy.ts";

const ref = (environmentId: string, threadId = "chat-1") => ({
  environmentId: EnvironmentId.make(environmentId),
  threadId: ThreadId.make(threadId),
});
const v1 = (threadId: string) => ({ environmentId: null, threadId: ThreadId.make(threadId) });
const local = EnvironmentId.make("local");
const titlesIn =
  (titles: Record<string, Record<string, string>>) =>
  ({ environmentId, threadId }: ScopedThreadRef) =>
    titles[environmentId]?.[threadId];
const japanese: MFunctionType = (encodedMsg, options) => {
  const english = mFallback(encodedMsg, options);
  return (
    english === "Thread no longer available" ? "スレッドは利用できなくなりました" : english
  ) as typeof english;
};

describe("thread link destinations", () => {
  it("names a v1 link's thread in the message's environment, as written and then decoded", () => {
    expect(threadLinkCandidates(v1("provider%3A1"), local)).toEqual([
      ref("local", "provider%3A1"),
      ref("local", "provider:1"),
    ]);
  });

  it("opens the unique matching chat in another environment for a v1 link", () => {
    const destination = ref("remote");
    expect(guessThreadLinkRef(v1("chat-1"), [ref("local", "other-chat"), destination])).toEqual(
      destination,
    );
  });

  it("opens a v2 link only in its own environment", () => {
    const there = ref("there", "a%3A1");
    expect(threadLinkCandidates(there, local)).toEqual([there, ref("there", "a:1")]);
    expect(guessThreadLinkRef(there, [ref("elsewhere", "a%3A1"), ref("elsewhere", "a:1")])).toBe(
      null,
    );
  });

  it("leaves ambiguous or missing destinations unresolved", () => {
    const link = v1("provider%3A1");
    const literal = ref("remote", "provider%3A1");
    const decoded = ref("remote", "provider:1");
    expect(
      guessThreadLinkRef(link, [literal, ref("other-remote", literal.threadId), decoded]),
    ).toBeNull();
    expect(guessThreadLinkRef(link, [decoded, ref("other-remote", decoded.threadId)])).toBeNull();
    expect(guessThreadLinkRef(link, [ref("remote", "other-chat")])).toBeNull();
  });
});

describe("thread links", () => {
  it("takes the ids verbatim, percent escapes included", () => {
    expect(parseThreadLinkHref("oni-thread://v1/mcp:1234")).toEqual(v1("mcp:1234"));
    expect(parseThreadLinkHref("oni-thread://v1/thread:delegated-task:mcp%3A1")).toEqual(
      v1("thread:delegated-task:mcp%3A1"),
    );
    expect(parseThreadLinkHref("oni-thread://v2/env-1/thread:a/b%3A1")).toEqual(
      ref("env-1", "thread:a/b%3A1"),
    );
  });

  it("rejects other links and an empty id", () => {
    expect(parseThreadLinkHref("https://github.com/aislopware/onicode")).toBeNull();
    expect(parseThreadLinkHref("oni-thread://v1/")).toBeNull();
    expect(parseThreadLinkHref("oni-thread://v1/ ")).toBeNull();
    expect(parseThreadLinkHref("oni-thread://v2/env-1")).toBeNull();
    expect(parseThreadLinkHref("oni-thread://v2/env-1/")).toBeNull();
    expect(parseThreadLinkHref("oni-thread://v2//chat-1")).toBeNull();
  });

  it("resolves a percent-encoded id when the id as written names no thread", () => {
    const title = titlesIn({ local: { "thread:project:1": "Decoded", "provider%3A1": "Literal" } });
    expect(
      relabelThreadLinks(
        "[a](oni-thread://v1/thread%3Aproject%3A1) [b](oni-thread://v1/provider%3A1)",
        local,
        title,
      ),
    ).toBe("[Decoded](oni-thread://v1/thread:project:1) [Literal](oni-thread://v1/provider%3A1)");
    const untitled = titlesIn({ local: { "a%3A1": "", "a:1": "Other" } });
    expect(relabelThreadLinks("[Kept](oni-thread://v1/a%3A1)", local, untitled)).toBe(
      "[Kept](oni-thread://v1/a%3A1)",
    );
  });

  it("titles a v2 link from its own environment and writes it with that title", () => {
    const title = titlesIn({ local: { t1: "Here" }, there: { t1: "There" } });
    expect(
      relabelThreadLinks(
        "<oni-thread://v2/there/t1> and [old](oni-thread://v2/local/t1)",
        local,
        title,
      ),
    ).toBe("[There](oni-thread://v2/there/t1) and [Here](oni-thread://v2/local/t1)");
  });

  it("reads an angle-bracketed destination as the labeled link, not an autolink", () => {
    const title = titlesIn({ local: { t1: "Here" }, there: { t1: "There" } });
    expect(
      relabelThreadLinks(
        "[old](<oni-thread://v2/there/t1>) and [older](<oni-thread://v2/local/t1>)",
        local,
        title,
      ),
    ).toBe("[There](oni-thread://v2/there/t1) and [Here](oni-thread://v2/local/t1)");
    expect(
      relabelThreadLinks("[Kept label](<oni-thread://v2/local/t1>)", local, () => undefined),
    ).toBe("Kept label");
    expect(threadLinkRefs("[a](<oni-thread://v2/there/t2>)", local)).toEqual([ref("there", "t2")]);
    for (const threadId of ["mcp:launch:qa(1)", "mcp:launch:qa)"]) {
      const thread = ref("there", threadId);
      const link = formatThreadLink(thread, "Original");
      expect(link).toBe(`[Original](<oni-thread://v2/there/${threadId}>)`);
      expect(threadLinkRefs(link, local)).toEqual([thread]);
      expect(relabelThreadLinks(link, local, () => "Renamed")).toBe(
        `[Renamed](<oni-thread://v2/there/${threadId}>)`,
      );
      expect(relabelThreadLinks(link, local, () => undefined)).toBe("Original");
    }
  });

  it("turns a v2 link it cannot resolve into plain text, never the raw link", () => {
    expect(
      relabelThreadLinks(
        "<oni-thread://v2/there/t1>, <oni-thread://v2/local/t1>, [Old title](oni-thread://v2/there/t1)",
        local,
        () => undefined,
      ),
    ).toBe("Thread on another machine, Thread no longer available, Old title");
    expect(relabelThreadLinks("[Old](oni-thread://v1/t1)", local, () => undefined)).toBe(
      "[Old](oni-thread://v1/t1)",
    );
    expect(relabelThreadLinks("See <oni-thread://v1/gone>", local, () => undefined, japanese)).toBe(
      "See スレッドは利用できなくなりました",
    );
  });

  it.each(["\r", "\r\n", "\n"])(
    "keeps BOM-prefixed thread link examples literal with %j line endings",
    (eol) => {
      const link = "<oni-thread://v2/local/t>";
      for (const fence of ["~~~", "```"]) {
        const example = [`\uFEFF${fence}`, link, fence].join(eol);
        expect(replaceThreadLinks(`${example}${eol}${link}`, () => "linked")).toBe(
          `${example}${eol}linked`,
        );
      }
    },
  );

  it.each([
    ["    ", ""],
    ["\t", ""],
    [">     ", "> "],
    ["-     ", ""],
  ])(
    "rewrites prose links after an indented code line that looks like a fence (%j)",
    (indent, prose) => {
      for (const bom of ["", "\uFEFF"]) {
        const example = `${bom}${indent}~~~\n${prose.trimEnd()}\n${prose}See `;
        expect(replaceThreadLinks(`${example}<oni-thread://v2/local/t>`, () => "linked")).toBe(
          `${example}linked`,
        );
      }
    },
  );

  it.each([
    ["a blank line", "Intro\n\n", "    ", "~~~", ""],
    ["an indented heading", "Intro\n  # Example\n", "    ", "~~~", ""],
    ["a closed fence", "Intro\n```\nexample\n```\n", "    ", "~~~", ""],
    ["a literal list marker", "# Example\n", "    ", "- ```md", ""],
    ["a heading in a quote", "> # Example\n", ">     ", "~~~", "> "],
  ])(
    "keeps indented code literal and rewrites prose after %s",
    (_name, before, indent, fence, quote) => {
      const inside = "<oni-thread://v2/local/code>";
      const outside = "<oni-thread://v2/local/prose>";
      const example = `${before}${indent}${fence}\n${indent}${inside}\n${quote.trimEnd()}\n${quote}See `;
      expect(replaceThreadLinks(`${example}${outside}`, () => "linked")).toBe(`${example}linked`);
    },
  );

  it.each([
    ["a paragraph with four spaces", "Intro\n    ~~~"],
    ["a paragraph with a tab", "Intro\n\t```"],
    ["a list paragraph", "- Intro\n      ~~~"],
    ["an HTML block", "<div>\n    ~~~\n</div>"],
    ["deeply nested quotes", ">".repeat(6000) + " text\n\n    ~~~"],
  ])("rewrites the prose link after %s", (_name, before) => {
    const prose = `${before}\n\nSee `;
    expect(replaceThreadLinks(`${prose}<oni-thread://v2/local/prose>`, () => "linked")).toBe(
      `${prose}linked`,
    );
  });

  it("rewrites a link in footnote prose before an indented fence", () => {
    const footnote = "Note[^n].\n\n[^n]: note\n\n    See ";
    expect(
      replaceThreadLinks(`${footnote}<oni-thread://v2/local/prose>\n\n    ~~~`, () => "linked"),
    ).toBe(`${footnote}linked\n\n    ~~~`);
  });

  it.each([
    ["a paragraph code span", "Intro\n    ```\n    <oni-thread://v2/local/code>\n    ```"],
    [
      "a fence with over-indented markers",
      "~~~\n    ~~~\n    ~~~\n<oni-thread://v2/local/code>\n~~~",
    ],
    [
      "a fence in a footnote",
      "Note[^n].\n\n[^n]: note\n\n    ~~~\n    <oni-thread://v2/local/code>\n    ~~~",
    ],
  ])("keeps links literal inside %s and rewrites the link after it", (_name, code) => {
    const prose = `${code}\n\nSee `;
    expect(replaceThreadLinks(`${prose}<oni-thread://v2/local/prose>`, () => "linked")).toBe(
      `${prose}linked`,
    );
  });

  it.each([
    ["4 spaces", "    ```md\n    <oni-thread://v1/abc>\n    ```", ""],
    ["tab", "\t```md\n\t<oni-thread://v1/abc>\n\t```", ""],
    ["list + 5 spaces", "-     ```md\n      <oni-thread://v1/abc>\n      ```", ""],
    ["nested list", "- a\n  -     ```md\n        <oni-thread://v1/abc>\n        ```", ""],
    ["nested list with tab", "-\t-     ~~~\n          <oni-thread://v1/abc>", ""],
    ["quote + 5 spaces", ">     ```md\n>     <oni-thread://v1/abc>\n>     ```", "> "],
    [
      "spaced nested quote + 5 spaces",
      ">    >     ```md\n>    >     <oni-thread://v1/abc>",
      ">    > ",
    ],
    ["quote + list + 5 spaces", "> -     ```md\n>       <oni-thread://v1/abc>\n>       ```", "> "],
    ["quote + space + tab + list", "> \t-     ```md\n>         <oni-thread://v1/abc>", "> "],
    [
      "quote + list after para",
      "> para\n> -     ```md\n>       <oni-thread://v1/abc>\n>       ```",
      "> ",
    ],
    ["1. after paragraph", "Intro\n1.     ```md\n       <oni-thread://v1/abc>\n       ```", ""],
    ["ordered list >= 2 after blank", "Intro\n\n2.     ```md\n       <oni-thread://v1/abc>", ""],
    [
      "quote after blank line past closer",
      "Example\n\n>       ~~~\n>       ~~~\n>       <oni-thread://v1/abc>",
      "> ",
    ],
    ["quote after autolink", "<https://example.com>\n>     ~~~\n>     <oni-thread://v1/abc>", "> "],
    ["blank line inside indented code", "    ```md\n\n    <oni-thread://v1/abc>", ""],
    ["quote after indented code", "    ```md\n>     ```\n>     <oni-thread://v1/abc>", "> "],
    ["4 spaces after closer", "    ```md\n    a\n    ```\n    <oni-thread://v1/abc>", ""],
    ["4 spaces with U+2028", "    ```\u2028<oni-thread://v1/t1>```", ""],
  ])(
    "keeps links inside fence-looking indented code blocks literal (%s)",
    (_name, block, prose) => {
      expect(replaceThreadLinks(block, () => "linked")).toBe(block);
      const after = `${block}\n${prose.trimEnd()}\n${prose}See `;
      expect(replaceThreadLinks(`${after}<oni-thread://v2/local/t>`, () => "linked")).toBe(
        `${after}linked`,
      );
    },
  );

  it("leaves links inside code spans and fences as written", () => {
    const markdown = [
      "Live [old](oni-thread://v1/t1), literal `[old](oni-thread://v1/t1)`.",
      "```md",
      "<oni-thread://v2/local/t1>",
      "```",
      "After <oni-thread://v2/local/t1>",
    ].join("\n");
    expect(relabelThreadLinks(markdown, local, () => "New")).toBe(
      [
        "Live [New](oni-thread://v1/t1), literal `[old](oni-thread://v1/t1)`.",
        "```md",
        "<oni-thread://v2/local/t1>",
        "```",
        "After [New](oni-thread://v2/local/t1)",
      ].join("\n"),
    );
    const nested = [
      "> > ```md",
      ">> <oni-thread://v2/local/t1>",
      ">>```",
      "> > <oni-thread://v2/local/t1>",
    ];
    expect(relabelThreadLinks(nested.join("\n"), local, () => "New")).toBe(
      [...nested.slice(0, 3), "> > [New](oni-thread://v2/local/t1)"].join("\n"),
    );
    const deepNested = "> > > > > ```\n> > > > > <oni-thread://v2/local/t1> ```";
    expect(relabelThreadLinks(deepNested, local, () => "New")).toBe(deepNested);
    const labels: Array<string | undefined> = [];
    const separated = "[Label\u2028> > example](oni-thread://v1/t1)";
    expect(replaceThreadLinks(separated, (_link, label) => void labels.push(label))).toBe(
      separated,
    );
    expect(labels).toEqual(["Label\u2028> > example"]);
    for (const escaped of [
      "\\``<oni-thread://v2/env-1/t1>`",
      "\\```<oni-thread://v2/env-1/t1>``",
    ]) {
      expect(threadLinkRefs(escaped, local)).toEqual([]);
    }
  });

  it("ends code where CommonMark does, so a link after it stays live", () => {
    const after = "See <oni-thread://v2/env-1/t1>";
    const hidden = "<oni-thread://v2/local/in-code>";
    for (const markdown of [
      ["```ts", 'const fence = "```";', hidden, "```", after],
      ["> ```ts", '> const fence = "```";', `> ${hidden}`, "> ```", `> ${after}`],
      ["> ```", ` > ${hidden}`, "> ```", after],
      ["- ```bash", "  echo hi", "  ```", "", after],
      ["- ```text", `  ${hidden}`, "", after],
      ["- item", "", "  ```text", `  ${hidden}`, "", after],
      ["[^note]: - ```text", `      ${hidden}`, "", after],
      ["[^outer]: [^inner]: - ```text", `          ${hidden}`, "", after],
      ["- > ```md", `  > ${hidden}`, "  > ```", "", after],
      [">> ```md", `> > ${hidden}`, ">> ```", `> ${after}`],
      ["> > ~~~", `>> ${hidden}`, ">> ~~~", after],
      ["1. Run:", "", "    ```bash", '    echo "```"', `    ${hidden}`, "    ```", "", after],
      ["````md", "```", hidden, "````", after],
      ["Use ~~~ to mark it.", after],
      ["> para", ">     ~~~", `>     ${after}`],
      ["> para", `>     ~~~ ${after}`],
      ["- > para", "  >     ```md", `  >     ${after}`],
      [`Intro\u2028-     ~~~ ${after}`],
      ["Intro", `2.     ~~~ ${after}`],
      ["Intro", `2. -     ~~~ ${after}`],
      [">\t-     ```md", `>       ${after}`],
      ["> para", "lazy", ">     ~~~", `>     ${after}`],
      ["> para", "> 2. >", `> 2.     ~~~ ${after}`],
      ["> para\r", ">     ~~~\r", `>     ${after}`],
      [`> para\r\r>     ~~~\r>     ${hidden}\r> ${after}`],
      ["para", "    -     ```", `    ${after}`],
      ["para", "    -     ```", `          ${after}`],
      ["    ~~~", `-   ${after}`],
      ["    ~~~md", "    ~~~", "    ~~~", after],
      ["- item", "  >     ~~~", `  >     ${hidden}`, `    ${after}`],
      ["10. >     ```md", `    >     ${hidden} \`\`\``, "", after],
      ["  - >     ```md", `    >     ${hidden} \`\`\``, "", after],
      ["> - Run this:", ">", ">     ```bash", ">   echo hi", ">", ">     ```", `>     ${after}`],
      ["Example", "", ">     ~~~", ">\t\t~~~", `>     ${hidden}`, ">\t\t~~~", `> ${after}`],
      [after, ">     ~~~", `>     ${hidden}`],
      [`1.       \`\`\`md ${hidden}`, `   \t${after}`],
      [">      ```md", `> \t${after}`],
      [">     ```md", `>\t ${after}`],
      ["```" + hidden + "``` then", after],
      ["Use ```example", hidden, "end``` then", after],
      ["Stray ` tick", "", after, "and `code`"],
      ["Stray ` tick", `# ${after}`, "and `code`"],
      ["Stray ` tick", `- ${after}`, "and `code`"],
      ["Stray ` tick", `> ${after} and \`code\``],
      ["> Stray ` tick", `> > ${after} and \`code\``],
      ["- Stray ` tick", `  > ${after} and \`code\``],
      ["> Use `example", `> ${hidden} end\``, `> ${after}`],
      ["> > Use `example", `> > ${hidden} end\``, `> > ${after}`],
      ["> Use `example", `${hidden} end\``, after],
      ["Use `example", `    > ${hidden} end\``, after],
      ["> Use `example", `>     > ${hidden} end\``, `> ${after}`],
      ["> Intro", "Use `example", `> ${hidden} end\``, `> ${after}`],
      ["> > Intro", "> Use `example", `> > ${hidden} end\``, `> > ${after}`],
      ["1. Stray ` tick", `    > ${after} and \`code\``],
      ["1. Intro", "", "    Stray ` tick", `    > ${after} and \`code\``],
      ["- Intro", "", "Use `example", `    > ${hidden} end\``, after],
      ...["- - -", "* \t* *", "___", "===", "-"].flatMap((boundary) => [
        ["Stray ` tick", boundary, `${after} and \`code\``],
        ["> Stray ` tick", `> ${boundary}`, `> ${after} and \`code\``],
      ]),
      ["Stray ` tick\r", "  ---\r", `${after} and \`code\``],
      ["> Stray ` tick", "---", `${after} and \`code\``],
      ["Use `example", "```" + hidden + "`", after],
      ["\\\\``" + after + "`"],
      [`Use \`example\r${hidden} end\`. Stray \` tick\r\r${after}\rand \`code\``],
      [`Stray \` tick\r# ${after}\rand \`code\``],
    ]) {
      const joined = markdown.join("\n");
      expect(threadLinkRefs(joined, local)).toEqual([ref("env-1", "t1")]);
      expect(relabelThreadLinks(joined, local, () => "New")).toBe(
        joined.replace("<oni-thread://v2/env-1/t1>", "[New](oni-thread://v2/env-1/t1)"),
      );
    }
  });

  it("keeps multiline code literal when a line only resembles a block boundary", () => {
    for (const markdown of [
      ...["--- text", "== =", "-_*", "    ==="].map((line) => ["Use `example", line]),
      ["> Use `example", ">     ---"],
      ["> Use `example", "==="],
    ]) {
      const source = [...markdown, "<oni-thread://v2/local/t1> end`"].join("\n");
      expect(threadLinkRefs(source, local)).toEqual([]);
      expect(relabelThreadLinks(source, local, () => "New")).toBe(source);
    }
  });

  it("lists the threads a message's links can name, outside code", () => {
    expect(
      threadLinkRefs(
        "<oni-thread://v2/there/t1> [a](oni-thread://v1/t%3A2) `<oni-thread://v2/code/onicode>`",
        local,
      ),
    ).toEqual([ref("there", "t1"), ref("local", "t%3A2"), ref("local", "t:2")]);
  });

  it("lists a message's thread links on runtimes whose matchAll omits named groups, such as Hermes", () => {
    const matchAll = String.prototype.matchAll;
    using _hermes = vi.spyOn(String.prototype, "matchAll").mockImplementation(function (
      this: string,
      regexp: RegExp,
    ) {
      return matchAll.call(this, regexp).map((match) => {
        delete match.groups;
        return match;
      });
    });
    expect(
      threadLinkRefs("<oni-thread://v2/local/t1> [a](oni-thread://v2/there/t2)", local),
    ).toEqual([ref("local", "t1"), ref("there", "t2")]);
  });

  it("writes a titleless autolink that opens that exact thread", () => {
    const link = formatThreadLink(ref("env-1", "thread:delegated-task:mcp%3A1"));
    expect(link).toBe("<oni-thread://v2/env-1/thread:delegated-task:mcp%3A1>");
    expect(parseThreadLinkHref(link.slice(1, -1))).toEqual(
      ref("env-1", "thread:delegated-task:mcp%3A1"),
    );
  });

  it("formats a label that would otherwise break the Markdown link", () => {
    expect(formatThreadLink(ref("e", "t1"), "Fix [ci] \\ build")).toBe(
      "[Fix ci build](oni-thread://v2/e/t1)",
    );
    expect(formatThreadLink(ref("e", "t1"), " ] ")).toBe("<oni-thread://v2/e/t1>");
  });

  it("leaves a thread link that is escaped or inside another link as written", () => {
    const autolink = "<oni-thread://v2/there/t1>";
    const relabeled = "[There](oni-thread://v2/there/t1)";
    const title = titlesIn({ there: { t1: "There" } });
    for (const [markdown, expected] of [
      [`[ ${autolink} ](https://example.com)`],
      [`[${autolink}](<https://example.com/a b> "Title")`],
      [`[a](https://example.com/x_(y)) ${autolink}`, `[a](https://example.com/x_(y)) ${relabeled}`],
      [`[[[ [old](oni-thread://v2/there/t1)`, `[[[ ${relabeled}`],
      [`\\\\[old](oni-thread://v2/there/t1)`, `\\\\${relabeled}`],
      [`\\[old](oni-thread://v2/there/t1) \\${autolink}`],
    ] as const) {
      expect(relabelThreadLinks(markdown, local, title)).toBe(expected ?? markdown);
      expect(threadLinkRefs(markdown, local)).toEqual(
        expected === undefined ? [] : [ref("there", "t1")],
      );
    }
  });

  it("keeps Markdown reference link definitions intact when relabeling", () => {
    // Onicode has no hosted links; its own thread links match only in angle brackets.
    const link = "oni-thread://v2/local/demo";
    for (const markdown of [
      `[Review][thread]\n\n[thread]: <${link}>`,
      `[Review][thread]\n\n[thread]:<${link}>`,
      `[Review][thread]\n\n[thread]:\n  <${link}>`,
      `[Review][thread]\n\n  [thread]: <${link}> "Demo"`,
      `> [Review][thread]\n>\n> [thread]: <${link}>`,
    ]) {
      for (const title of [() => "Demo", () => undefined]) {
        expect(relabelThreadLinks(markdown, local, title)).toBe(markdown);
      }
    }

    const withLiveAfter = `[Review][thread]\n\n[thread]: ${link}\n\nSee <${link}>`;
    expect(relabelThreadLinks(withLiveAfter, local, () => "Demo")).toBe(
      `[Review][thread]\n\n[thread]: ${link}\n\nSee [Demo](${link})`,
    );

    const paragraphContinuation = `Intro\n[thread]: <${link}>`;
    expect(relabelThreadLinks(paragraphContinuation, local, () => "Demo")).toBe(
      `Intro\n[thread]: [Demo](${link})`,
    );
  });

  it("keeps relabeled links navigable when the thread id contains a space", () => {
    const linkUrl = (markdown: string) => {
      const paragraph = unified().use(remarkParse).parse(markdown).children[0];
      const node = paragraph?.type === "paragraph" ? paragraph.children[0] : undefined;
      return node?.type === "link" ? node.url : undefined;
    };
    const thread = ref("local", "work item");
    const titles = titlesIn({ local: { "work item": "Review" } });

    const relabeled = relabelThreadLinks(formatThreadLink(thread), local, titles);
    expect(linkUrl(relabeled)).toBe("oni-thread://v2/local/work%20item");
    expect(threadLinkRefs(relabeled, local)).toContainEqual(thread);

    const v1Relabeled = relabelThreadLinks("[old](oni-thread://v1/work%20item)", local, titles);
    expect(linkUrl(v1Relabeled)).toBe("oni-thread://v1/work%20item");
    expect(formatThreadLink(v1("work item"))).toBe("<oni-thread://v1/work%20item>");
  });
});
