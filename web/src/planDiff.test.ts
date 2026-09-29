import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { PlanBody } from "@/components/PlanView";
import { planChanges } from "@/planDiff";

describe("planChanges", () => {
  it("diffs a changed line word by word", () => {
    expect(planChanges("a b c\n", "a x c\n")).toEqual({
      added: [[2, 3]],
      removals: [{ at: 2, text: "b", lines: false }],
    });
  });

  it("keeps whole removed lines and whole new lines", () => {
    expect(planChanges("a\nb\nc\n", "a\nc\n")).toEqual({
      added: [],
      removals: [{ at: 2, text: "b", lines: true }],
    });
    expect(planChanges("a\n", "a\nb c\n")).toEqual({ added: [[2, 5]], removals: [] });
  });

  it("word-diffs only the lines edited in place, next to removed lines", () => {
    const previous =
      "5. Render a brand new step, added in this version.\n\n" +
      "## Added section\nThis whole section is new, so it should be tinted.\n\n## End\n";
    const text = "5. Render a step, added in the second version.\n\n## End\n";
    expect(planChanges(previous, text)).toEqual({
      added: [[27, 37]],
      removals: [
        { at: 12, text: "brand new", lines: false },
        { at: 27, text: "this", lines: false },
        {
          at: 47,
          text: "## Added section\nThis whole section is new, so it should be tinted.",
          lines: true,
        },
      ],
    });
  });

  it("ignores whitespace-only changes", () => {
    expect(planChanges("a\n\n\nb\n", "a\n\nb\n")).toEqual({ added: [], removals: [] });
  });
});

describe("PlanBody", () => {
  const render = (previous: string, text: string) =>
    renderToStaticMarkup(createElement(PlanBody, { text, previous })).replace(/\n/g, "");
  const mark = (kind: "dot" | "line") =>
    `<span class="plan-mark plan-mark-${kind}"><button type="button" aria-label="Show removed text" aria-expanded="false" class="${kind === "dot" ? "plan-dot" : "plan-cut"}"></button></span>`;

  it("tints new words and puts a dot where words went", () => {
    expect(render("Keep this line.\n", "Keep that line.\n")).toBe(
      `<div class="markdown"><p>Keep ${mark("dot")}<span class="plan-added">that</span> line.</p></div>`,
    );
  });

  it("draws a line where a block went", () => {
    expect(render("one\n\ntwo\n\nthree\n", "one\n\nthree\n")).toBe(
      `<div class="markdown"><p>one</p>${mark("line")}<p>three</p></div>`,
    );
  });

  it("draws a list item's removal at the top of the item after it", () => {
    expect(render("- a\n- b\n- c\n", "- a\n- c\n")).toBe(
      `<div class="markdown"><ul><li>a</li><li>${mark("line")}c</li></ul></div>`,
    );
  });

  it("tints changed code lines and marks lost words at the line's end", () => {
    const html = render("```\nlet a = 1;\n```\n", "```\nlet a = 2;\nlet b = 3;\n```\n");
    expect(html).toBe(
      `<div class="markdown"><pre><code>` +
        `<span class="plan-code-line plan-added">let a = 2;${mark("dot")}</span>` +
        `<span class="plan-code-line plan-added">let b = 3;</span>` +
        `</code></pre></div>`,
    );
  });

  it("renders a first review without marks", () => {
    expect(renderToStaticMarkup(createElement(PlanBody, { text: "# A", previous: null }))).toBe(
      '<div class="markdown"><h1>A</h1></div>',
    );
  });
});
