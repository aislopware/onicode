import { describe, expect, it } from "vite-plus/test";

import { restorePastedHtmlLinks } from "./pastedMarkdownLinks.ts";

describe("restoring pasted Markdown links from clipboard HTML", () => {
  it.each([
    {
      case: "repeated labels with distinct destinations",
      plain: "Guide: Guide, Guide.",
      html: '<p>Guide: <a href="https://example.com/start">Guide</a>, <a href="https://example.com/end">Guide</a>.</p>',
      expected: "Guide: [Guide](https://example.com/start), [Guide](https://example.com/end).",
    },
    {
      case: "line breaks within a link",
      plain: "First\nSecond",
      html: '<a href="https://example.com/">First<br>Second</a>',
      expected: "[First](https://example.com/)\n[Second](https://example.com/)",
    },
    {
      case: "a backslash immediately before a link",
      plain: "\\Guide",
      html: '\\<a href="https://example.com/">Guide</a>',
      expected: "\\\\[Guide](https://example.com/)",
    },
    {
      case: "nested labels, punctuation and native clipboard line breaks",
      plain: "First line\n\nRead [the guide] & notes",
      html: '<p>First line</p><p>Read <a href="https://example.com/guide(part)?a=1&amp;b=2"><strong>[the guide]</strong></a> &amp; notes</p>',
      expected:
        "First line\n\nRead [\\[the guide\\]](https://example.com/guide\\(part\\)?a=1&amp;b=2) & notes",
    },
    {
      case: "visible labels surrounded by hidden content",
      plain: "Read the guide.",
      html: '<span hidden>Skip</span>Read <a href="https://example.com/guide">the <span style="display: none">hidden </span>guide</a>.',
      expected: "Read [the guide](https://example.com/guide).",
    },
    {
      case: "visible link overrides inside a hidden ancestor",
      plain: "Guide",
      html: '<span style="visibility:hidden">Skip <a style="visibility:visible" href="https://example.com/"><span style="display:none;visibility:visible">Skip </span>Guide</a></span>',
      expected: "[Guide](https://example.com/)",
    },
    {
      case: "destinations without an external link scheme and anchors with no label",
      plain: "Local, unavailable, and !Guide",
      html: '<a href="/local">Local</a>, <a>unavailable</a>, and !<a href="https://example.com/guide">Guide</a><a href="https://example.com/empty"></a>',
      expected: "Local, unavailable, and \\![Guide](https://example.com/guide)",
    },
    {
      case: "linked identifiers inside code samples as code",
      plain: "pub fn new(v: Vec<u8>) -> Self",
      html: '<pre><code>pub fn new(v: <a href="https://doc.rust-lang.org/std/vec/struct.Vec.html">Vec</a>&lt;<a href="https://doc.rust-lang.org/std/primitive.u8.html">u8</a>&gt;) -&gt; Self</code></pre>',
      expected: "pub fn new(v: Vec<u8>) -> Self",
    },
    {
      case: "literal entity names in the destination",
      plain: "Guide",
      html: '<a href="https://example.com/?q=&amp;copy;">Guide</a>',
      expected: "[Guide](https://example.com/?q=&amp;copy;)",
    },
    {
      case: "a browser's clipboard wrapper, comments, scripts and styles",
      plain: "Read Setup guide now",
      html: "<html><head><meta charset='utf-8'><style>a::after { content: '<a href=x>' }</style></head><body><!--StartFragment-->Read <a href='https://example.com/setup'>Setup guide</a><script>document.write('<a href=\"https://evil.example\">x</a>')</script> now<!--EndFragment--></body></html>",
      expected: "Read [Setup guide](https://example.com/setup) now",
    },
    {
      case: "unquoted attributes, character references and a mail link",
      plain: "It’s Ada\u00a0Lovelace — write ada@example.com",
      html: 'It&rsquo;s <a href=https://example.com/ada title=x>Ada&nbsp;Lovelace</a> &#8212; write <A HREF="mailto:ada@example.com">ada@example.com</A>',
      expected:
        "It’s [Ada\u00a0Lovelace](https://example.com/ada) — write [ada@example.com](mailto:ada@example.com)",
    },
    {
      case: "an anchor the next one implicitly closes",
      plain: "One Two",
      html: '<a href="https://example.com/1">One <a href="https://example.com/2">Two</a>',
      expected: "[One](https://example.com/1) [Two](https://example.com/2)",
    },
    {
      case: "a Windows path as a label",
      plain: String.raw`C:\Users\me\ and C:\app\[id]`,
      html: String.raw`<a href="https://example.com/1">C:\Users\me\</a> and <a href="https://example.com/2">C:\app\[id]</a>`,
      expected: String.raw`[C:\Users\me\\](https://example.com/1) and [C:\app\[id\]](https://example.com/2)`,
    },
    {
      case: "a label that looks like another link",
      plain: "C:/x](https://evil.example)",
      html: '<a href="https://example.com/safe">C:/x](https://evil.example)</a>',
      expected: "[C:/x\\](https://evil.example)](https://example.com/safe)",
    },
    {
      case: "destinations without a host, as written",
      plain: "Empty, spaced, fragment, mail",
      html: '<a href="https://">Empty</a>, <a href="https://not a host/path">spaced</a>, <a href="https://example.com#a#b">fragment</a>, <a href="mailto:ada@example.com#sig">mail</a>',
      expected:
        "Empty, spaced, [fragment](https://example.com#a#b), [mail](mailto:ada@example.com#sig)",
    },
    {
      case: "a link after a hidden anchor it implicitly closes",
      plain: "Visible",
      html: '<a href="https://example.com/1" style="display:none">Hidden <a href="https://example.com/2">Visible</a>',
      expected: "[Visible](https://example.com/2)",
    },
    {
      case: "bare carriage returns as line breaks",
      plain: "First\r\rSecond",
      html: '<a href="https://example.com/">First<br><br>Second</a>',
      expected: "[First](https://example.com/)\n\n[Second](https://example.com/)",
    },
    {
      case: "HTML that does not describe the plain text",
      plain: "Something else",
      html: '<a href="https://example.com/">Guide</a>',
      expected: "Something else",
    },
  ])("keeps $case", ({ plain, html, expected }) => {
    expect(restorePastedHtmlLinks(plain, html)).toBe(expected);
  });
});
