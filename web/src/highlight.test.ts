import { describe, expect, it } from "vitest";

import {
  highlight,
  highlightLines,
  languageFromPath,
  languageFromTag,
  splitHighlighted,
} from "@/highlight";

describe("languages", () => {
  it("maps fence tags, case and aliases included", () => {
    expect(languageFromTag("rust")).toBe("rust");
    expect(languageFromTag("TSX")).toBe("typescript");
    expect(languageFromTag("html")).toBe("xml");
    expect(languageFromTag("toml")).toBe("ini");
    expect(languageFromTag("sh")).toBe("bash");
    expect(languageFromTag("md")).toBe("markdown");
    expect(languageFromTag("cobol")).toBeNull();
    expect(languageFromTag("")).toBeNull();
  });

  it("maps file extensions", () => {
    expect(languageFromPath("src/main.rs")).toBe("rust");
    expect(languageFromPath("web\\src\\components\\Diff.tsx")).toBe("typescript");
    expect(languageFromPath("Cargo.toml")).toBe("ini");
    expect(languageFromPath("index.HTML")).toBe("xml");
    expect(languageFromPath("script.py")).toBe("python");
    expect(languageFromPath("Makefile")).toBeNull();
    expect(languageFromPath(".gitignore")).toBeNull();
    expect(languageFromPath("dir.rs/notes")).toBeNull();
  });
});

describe("highlight", () => {
  it("wraps tokens and escapes text", () => {
    const html = highlight('let a = "<b>";', "rust");
    expect(html).toContain('<span class="hljs-keyword">let</span>');
    expect(html).toContain("&lt;b&gt;");
  });

  it("returns null for an unknown or missing language", () => {
    expect(highlight("x", null)).toBeNull();
    expect(highlight("x", "cobol")).toBeNull();
    expect(highlightLines("x", null)).toBeNull();
  });

  it("splits into self-contained lines", () => {
    expect(splitHighlighted('a<span class="c">b\nc</span>d\ne')).toEqual([
      'a<span class="c">b</span>',
      '<span class="c">c</span>d',
      "e",
    ]);
    expect(splitHighlighted('<span class="x"><span class="y">1\n2</span>\n3</span>')).toEqual([
      '<span class="x"><span class="y">1</span></span>',
      '<span class="x"><span class="y">2</span></span>',
      '<span class="x">3</span>',
    ]);
    expect(splitHighlighted("")).toEqual([""]);
    expect(splitHighlighted("a\n")).toEqual(["a", ""]);
  });

  it("keeps a multi-line comment colored on every line", () => {
    const lines = highlightLines("/* one\ntwo */\nlet x = 1;", "typescript");
    expect(lines).toHaveLength(3);
    expect(lines?.[1]).toBe('<span class="hljs-comment">two */</span>');
  });
});
