import { expect, it } from "vite-plus/test";
import { collectMobileAssistantTextSelectors } from "./assistantCitationSelection";
import {
  findAssistantCitationText,
  serializeAssistantCitation,
  expandAssistantCitationsForProvider,
} from "@onicode/shared/assistantCitations";
import { EnvironmentId, ThreadId, MessageId } from "@onicode/contracts";

it("offers distinct source choices within a block and maps recursive folds without merging occurrences", () => {
  const paragraph = `${"x".repeat(40)}**quote**${"x".repeat(40)}`;
  const markdown = `${paragraph}\n\n<details><summary>Outer</summary>\n\n<details><summary>Inner</summary>\n\n${paragraph}\n\n</details>\n\n</details>`;
  const [first] = collectMobileAssistantTextSelectors(markdown, "0:0", "quote", false);
  const [second] = collectMobileAssistantTextSelectors(markdown, "1/1/0:0", "quote", false);
  expect([first?.index, first?.count, second?.index, second?.count]).toEqual([0, 2, 1, 2]);
  const rendered = `${"x".repeat(40)}quote${"x".repeat(40)} ${"x".repeat(40)}quote${"x".repeat(40)}`;
  expect(second?.selector.start).toBe(rendered.lastIndexOf("quote"));
  // Identical context on both sides stays ambiguous rather than guessing.
  expect(findAssistantCitationText(rendered, second!.selector)).toBeNull();
  const markers = [first!, second!].map(({ selector }) =>
    serializeAssistantCitation({
      version: 1,
      environmentId: EnvironmentId.make("env"),
      threadId: ThreadId.make("thread"),
      messageId: MessageId.make("assistant"),
      ...selector,
    }),
  );
  expect(markers[0]).not.toBe(markers[1]);
  expect(expandAssistantCitationsForProvider(markers.join(" "))).toContain(
    '"id": "assistant-quote-2"',
  );
  const [reference] = collectMobileAssistantTextSelectors(
    "before [label][id] after\n\n<details><summary>More</summary>\n\n[id]: https://example.com\n\n</details>",
    "0:0",
    "before label after",
    false,
  );
  expect(reference?.selector.text).toBe("before label after");
  const sameBlock = collectMobileAssistantTextSelectors(
    "first **quote** then **quote** last",
    "0:0",
    "quote",
    false,
  );
  expect(sameBlock[1]?.selector.start).toBe(17);
  expect(sameBlock[1]?.selector.prefix).toBe("first quote then ");
  const [leadingSpace] = collectMobileAssistantTextSelectors(
    "x\n\n<details><summary>More</summary>\n\n` quote`\n\n</details>",
    "1/0:0",
    "quote",
    false,
  );
  expect(leadingSpace?.selector.start).toBe(2);
  expect(leadingSpace?.count).toBe(1);
  const codeChoices = collectMobileAssistantTextSelectors(
    "quote\n\n```text\nquote\n```\n\nquote",
    "0:0",
    "quote",
    false,
  );
  expect(codeChoices.map((choice) => [choice.index, choice.count])).toEqual([
    [0, 3],
    [1, 3],
    [2, 3],
  ]);
  for (const { source, path } of [
    { source: "![before](image.png)\n\nquote quote\n\n![after](image.png)", path: "0:0" },
    {
      source: 'quote\n\n```mermaid\nflowchart TD\nA["quote"]\n```\n\nquote',
      path: "0:0",
    },
    {
      source:
        "![before](image.png)\n\n<details><summary>More</summary>\n\nquote quote\n\n</details>\n\n![after](image.png)",
      path: "1/0:0",
    },
  ]) {
    const selectors = collectMobileAssistantTextSelectors(source, path, "quote", false);
    expect(selectors).toHaveLength(2);
    for (const [index, { selector }] of selectors.entries()) {
      expect(findAssistantCitationText("quote quote", selector)).toEqual({
        start: index * 6,
        end: index * 6 + 5,
      });
    }
  }
});

it("rejects unmappable or blank selections instead of inventing a source", () => {
  expect(collectMobileAssistantTextSelectors("quote quote", "0:0", " ", false)).toEqual([]);
  expect(collectMobileAssistantTextSelectors("quote ".repeat(21), "0:0", "quote", false)).toEqual(
    [],
  );
  expect(collectMobileAssistantTextSelectors("quote quote", "0:0", "other", false)).toEqual([]);
  expect(collectMobileAssistantTextSelectors("quote quote", "missing", "quote", false)).toEqual([]);
});

it("uses the web math representation around a selected passage", () => {
  for (const formula of ["$x$", "\\(x\\)"]) {
    const [choice] = collectMobileAssistantTextSelectors(
      `Before ${formula} after`,
      "0:0",
      "after",
      true,
    );
    expect(choice?.selector.prefix).toBe("Before \\(x\\) ");
    expect(findAssistantCitationText("Before \\(x\\) after", choice!.selector)).toEqual({
      start: 13,
      end: 18,
    });
  }
  const [display] = collectMobileAssistantTextSelectors(
    "Before\n\n$$\nx\n$$\n\nafter",
    "0:0",
    "after",
    true,
  );
  expect(display?.selector.prefix).toBe("Before \\[ x \\] ");
  // Native selections carry each rendered inline formula as U+FFFC.
  const [across] = collectMobileAssistantTextSelectors(
    "Let $a$ and \\(b\\) be",
    "0:0",
    "Let \uFFFC and \uFFFC",
    true,
  );
  expect(across?.selector.text).toBe("Let \\(a\\) and \\(b\\)");
  expect(findAssistantCitationText("Let \\(a\\) and \\(b\\) be", across!.selector)).toEqual({
    start: 0,
    end: 19,
  });
  const [lines] = collectMobileAssistantTextSelectors(
    "First $a$ line.\n\nSecond",
    "0:0",
    "First \uFFFC line.\nSecond",
    true,
  );
  expect(lines?.selector.text).toBe("First \\(a\\) line.\nSecond");
  const [literal] = collectMobileAssistantTextSelectors(
    "a \uFFFC lit $x$ then b",
    "0:0",
    "then b",
    true,
  );
  expect(literal?.selector.prefix).toBe("a \uFFFC lit \\(x\\) ");
  const [uncertainLiteral] = collectMobileAssistantTextSelectors(
    "a \uFFFC lit 2$x$ then b",
    "0:0",
    "then b",
    true,
  );
  expect(uncertainLiteral?.selector.prefix).toBe("a \uFFFC lit 2\\(x\\) ");
  // When md4c and this rebuild could disagree on formulas, a quote may match another occurrence.
  for (const source of [
    "Use $ x $ here.\n\nUse $y$ here.",
    "<div>Use $x$ here.</div>\n\nUse $y$ here.",
    "Before $$Use $x$ here$$.\n\nUse $z$ here.",
  ]) {
    expect(collectMobileAssistantTextSelectors(source, "0:0", "Use \uFFFC here", true)).toEqual([]);
  }
  expect(
    collectMobileAssistantTextSelectors("Use $\\($\\)( here.", "0:0", "Use \uFFFC)( here.", true),
  ).toEqual([]);
  // md4c only opens or closes `$` beside whitespace or punctuation, so these merge natively.
  expect(collectMobileAssistantTextSelectors("已知 $x$和$y$。", "0:0", "\uFFFC。", true)).toEqual(
    [],
  );
  expect(
    collectMobileAssistantTextSelectors(
      "see $a$b$c$ now\n\nsee $q$ now",
      "0:0",
      "see \uFFFC now",
      true,
    ),
  ).toEqual([]);
  // md4c reads the text after inline HTML is stripped, where these formulas merge.
  expect(
    collectMobileAssistantTextSelectors(
      "设 $x$<em>及</em>$y$ 为常数。\n\n设 $z$ 为常数。",
      "0:0",
      "设 \uFFFC 为常数。",
      true,
    ),
  ).toEqual([]);
  // An emoji is punctuation to md4c, so this formula renders.
  const [emoji] = collectMobileAssistantTextSelectors(
    "Use $x$\u{1F680} now.",
    "0:0",
    "Use \uFFFC\u{1F680}",
    true,
  );
  expect(emoji?.selector.text).toBe("Use \\(x\\)\u{1F680}");
  // Display math on its own line is a separate native view, never part of a text selection.
  expect(
    collectMobileAssistantTextSelectors(
      "Use $x$ here.\n\n$$\ny\n$$\n\nThen $$z$$\nend.",
      "0:0",
      "\uFFFC",
      true,
    ).map((choice) => choice.selector.text),
  ).toEqual(["\\(x\\)"]);
  // md4c only moves display math that is a paragraph's own child, beside an edge or a separator.
  expect(
    collectMobileAssistantTextSelectors(
      "**$$a$$** is big. Also $b$ is big.",
      "0:0",
      "\uFFFC is big",
      true,
    ).map((choice) => choice.selector.text),
  ).toEqual(["\\[\na\n\\]", "\\(b\\)"].map((tex) => `${tex} is big`));
  expect(
    collectMobileAssistantTextSelectors(
      "**Step:** $$x$$ so $y$ so it.",
      "0:0",
      "\uFFFC so",
      true,
    ).map((choice) => choice.selector.text),
  ).toEqual(["\\(y\\) so"]);
  // md4c ends the paragraph, and so the formula, at a blank line.
  for (const source of [
    "$$\na\n\nb\n$$",
    "$$\r\na\r\n\r\nb\r\n$$",
    "> $$\n> a\n>\n> b\n> $$",
    "$$\na\n- b\n$$",
  ]) {
    const [blank] = collectMobileAssistantTextSelectors(source, "0:0", "a", true);
    expect(blank?.selector.prefix).toBe("\\[ ");
  }
  const texts = (markdown: string, quote: string) =>
    collectMobileAssistantTextSelectors(markdown, "0:0", quote, true).map(
      (choice) => choice.selector.text,
    );
  // A math fence never selects as U+FFFC, and md4c splits table cells before reading `$`.
  expect(texts("> ```math\n> x\n> ```\n\nUse $y$ here.", "\uFFFC")).toEqual(["\\(y\\)"]);
  expect(texts("| a | b |\n|---|---|\n| $|x|$ | $y$ |", "\uFFFC")).toEqual([]);
  expect(texts("| a |\n|---|\n| \\(|x|\\) |\n\nUse $y$ here.", "Use \uFFFC here")).toEqual([
    "Use \\(y\\) here",
  ]);
  expect(texts("| a | b |\n|---|---|\n| $x\\\\|y$ | $z$ |", "\uFFFC")).toEqual([]);
  expect(texts("| a |\n|---|\n| $x\\|y$ |\n\nUse $z$ here.", "Use \uFFFC here")).toEqual([
    "Use \\(z\\) here",
  ]);
  // md4c drops a spoiler's pipes from the text and leaves footnote references literal.
  expect(texts("Use ||$x$|| here. Use $y$ here.", "Use \uFFFC here")).toEqual([]);
  expect(texts("See [^1] $$x$$ holds. Also $y$ holds.\n\n[^1]: See it.", "\uFFFC holds")).toEqual(
    [],
  );
  // In a list, a math fence shows as its code text.
  expect(texts("We set x = 1 first.\n\n1. Then:\n   ```math\n   x = 1\n   ```", "x = 1")).toEqual([
    "x = 1",
    "x = 1",
  ]);
  // md4c may keep a bare URL glued to text as text, so this display math may stay inline.
  expect(texts("参见https://example.com $$E$$ 成立。另外 $p$ 成立。", "\uFFFC 成立")).toEqual([
    "\\[\nE\n\\] 成立",
    "\\(p\\) 成立",
  ]);
  // A no-break space does not separate display math for md4c.
  expect(texts("**Note:**\u00a0$$x$$ holds. Also $y$ holds.", "\uFFFC holds")).toEqual([
    "\\[\nx\n\\] holds",
    "\\(y\\) holds",
  ]);
  expect(texts("a &#57346; b $x$ c", "\ue002 b \uFFFC")).toEqual([]);
  // A backticked file path after `!` renders as an inline image natively.
  expect(
    collectMobileAssistantTextSelectors(
      "Open !`/workspace/repo/diagram.png` here. Open $x$ here.",
      "0:0",
      "Open \uFFFC here",
      true,
    ),
  ).toEqual([]);
  // md4c leaves `2$x + y$` and `$n$th` literal, but the web renders them as formulas.
  for (const [source, web, quote, starts] of [
    [
      "The result is 2$x + y$. The input is x + y.",
      "The result is 2\\(x + y\\). The input is x + y.",
      "x + y",
      [17, 39],
    ],
    [
      "The $n$th term and the $m$th term differ.",
      "The \\(n\\)th term and the \\(m\\)th term differ.",
      "th term",
      [9, 30],
    ],
  ] as const) {
    const choices = collectMobileAssistantTextSelectors(source, "0:0", quote, true);
    expect(choices.map((choice) => findAssistantCitationText(web, choice.selector))).toEqual(
      starts.map((start) => ({ start, end: start + quote.length })),
    );
  }
  const [shell] = collectMobileAssistantTextSelectors(
    "Use $a$ here.\n\n```sh\n$ npm i\n```",
    "0:0",
    "Use \uFFFC here",
    true,
  );
  expect(shell?.selector.text).toBe("Use \\(a\\) here");
  // Images and rules are U+FFFC natively too, so a selection may not stand for a formula.
  for (const source of [
    "Use ![i](x.png) here. Use $x$ here.",
    "Use\n\n---\n\nhere\n\nUse $x$ here",
  ]) {
    expect(collectMobileAssistantTextSelectors(source, "0:0", "Use \uFFFC here", true)).toEqual([]);
  }
});

it("cites formulas as the literal text shown when math rendering is off", () => {
  const [inline] = collectMobileAssistantTextSelectors(
    "Before $x$ after",
    "0:0",
    "Before $x$ after",
    false,
  );
  expect(findAssistantCitationText("Before $x$ after", inline!.selector)).toEqual({
    start: 0,
    end: 16,
  });
  const [escaped] = collectMobileAssistantTextSelectors(
    "Before \\(x\\) after",
    "0:0",
    "after",
    false,
  );
  expect(escaped?.selector.prefix).toBe("Before (x) ");
  const [display] = collectMobileAssistantTextSelectors(
    "Before\n\n$$\nx\n$$\n\nafter",
    "0:0",
    "after",
    false,
  );
  expect(display?.selector.prefix).toBe("Before $$ x $$ ");
  const [authored] = collectMobileAssistantTextSelectors(
    "Use \uFFFC here\n\n---",
    "0:0",
    "Use \uFFFC here",
    false,
  );
  expect(authored?.selector.text).toBe("Use \uFFFC here");
  const [privateUse] = collectMobileAssistantTextSelectors(
    "a \ue002 b \uFFFC c",
    "0:0",
    "\ue002 b \uFFFC",
    false,
  );
  expect(privateUse?.selector.text).toBe("\ue002 b \uFFFC");
});
