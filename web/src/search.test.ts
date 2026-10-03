import { describe, expect, it } from "vitest";

import { markWords, searchWords } from "@/search";

describe("searchWords", () => {
  it("splits on any whitespace", () => {
    expect(searchWords("  prettier\tconfig \n")).toEqual(["prettier", "config"]);
    expect(searchWords("   ")).toEqual([]);
  });
});

describe("markWords", () => {
  it("marks every word, ignoring case", () => {
    expect(markWords("The Prettier config, prettier.", ["prettier", "CONFIG"])).toEqual([
      { text: "The ", marked: false },
      { text: "Prettier", marked: true },
      { text: " ", marked: false },
      { text: "config", marked: true },
      { text: ", ", marked: false },
      { text: "prettier", marked: true },
      { text: ".", marked: false },
    ]);
  });

  it("takes words as plain text", () => {
    expect(markWords("a (b) c", ["(b)"])).toEqual([
      { text: "a ", marked: false },
      { text: "(b)", marked: true },
      { text: " c", marked: false },
    ]);
  });

  it("leaves the text whole without words", () => {
    expect(markWords("text", [])).toEqual([{ text: "text", marked: false }]);
  });
});
