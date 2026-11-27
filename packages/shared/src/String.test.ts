import { describe, expect, it } from "vite-plus/test";

import { truncate, wordCutEnd, wordCutStart } from "./String.ts";

describe("truncate", () => {
  it("cuts by Unicode code point so emoji stay whole", () => {
    expect(truncate("👍👍👍", 5)).toBe("👍👍👍");
    expect(truncate("👍👍👍👍", 3)).toBe("👍👍👍...");
  });

  it("trims before measuring", () => {
    expect(truncate("  hello  ", 5)).toBe("hello");
  });

  it("drops whitespace at the cut before the ellipsis", () => {
    expect(truncate("hello world", 6)).toBe("hello...");
  });
});

describe("wordCutEnd", () => {
  it("cuts at the last space unless that drops more than half", () => {
    expect(wordCutEnd("one two three four", 10)).toBe(7);
    // An early space, as in `**结论：** ` before text with no spaces, is not a word end.
    const said = `**结论：** ${"这个问题出现在解析器中。".repeat(5)}`;
    expect(wordCutEnd(said, 20)).toBe(20);
    expect(wordCutEnd("😀".repeat(10), 9)).toBe(8);
    expect(wordCutEnd("hello world ok", 14)).toBe(14);
    expect(wordCutEnd("hi", 10)).toBe(2);
    expect(wordCutEnd(`hello world   ${"x".repeat(20)}`, 13)).toBe(11);
    expect(wordCutEnd(`first line\n${"y".repeat(20)}`, 14)).toBe(10);
    expect(wordCutEnd("hi 😀x", 4)).toBe(2);
  });
});

describe("wordCutStart", () => {
  it("starts after the first space unless that drops more than half", () => {
    expect(wordCutStart("one two three four", 10)).toBe(8);
    // Starting after "three" would keep only 4 of 9.
    expect(wordCutStart("one two three four", 9)).toBe(9);
    expect(wordCutStart("more   hello world", 12)).toBe(7);
    expect(wordCutStart("x😀 hi", 4)).toBe(4);
    const text = `${"这个问题出现在解析器中。".repeat(5)} 末尾`;
    expect(wordCutStart(text, 20)).toBe(text.length - 20);
    expect(wordCutStart("😀".repeat(10), 9)).toBe(12);
  });
});
