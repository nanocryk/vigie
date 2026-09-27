import { describe, expect, it } from "vitest";

import {
  type DiffLine,
  type Hunk,
  changeSummary,
  gutterWidth,
  highlightHunk,
  lineDiff,
  parsePatch,
  splitLines,
  toolDiff,
} from "@/diff";
import { type Item, applyEvents, emptyTranscript } from "@/transcript";
import { type WireEvent, parseHostEvent } from "@/wire";

const SESSION = "00000000-0000-0000-0000-000000000002";
const CWD = "/home/dev/project";
const TIME = "2026-09-27T10:00:00.000Z";

/** A session in `CWD` where Claude calls `name` with `input`, answered by `result` (unless omitted). */
function toolEvents(
  name: string,
  input: unknown,
  answer?: { content: string; isError?: boolean; result?: unknown },
): WireEvent[] {
  const payloads: { kind: "host" | "line"; payload: unknown }[] = [
    { kind: "host", payload: { event: "started", cwd: CWD } },
    {
      kind: "line",
      payload: {
        type: "assistant",
        parent_tool_use_id: null,
        message: {
          id: "msg_1",
          content: [{ type: "tool_use", id: "toolu_1", name, input }],
        },
      },
    },
  ];
  if (answer !== undefined) {
    payloads.push({
      kind: "line",
      payload: {
        type: "user",
        parent_tool_use_id: null,
        message: {
          role: "user",
          content: [
            {
              type: "tool_result",
              tool_use_id: "toolu_1",
              content: answer.content,
              is_error: answer.isError === true,
            },
          ],
        },
        ...(answer.result === undefined ? {} : { tool_use_result: answer.result }),
      },
    });
  }
  return payloads.map(({ kind, payload }, index) => {
    const base = { session: SESSION, seq: index + 1, time: TIME };
    return kind === "host"
      ? { ...base, kind, payload: parseHostEvent(payload) }
      : { ...base, kind, payload };
  });
}

function toolItem(events: WireEvent[]): Extract<Item, { kind: "tool" }> {
  const item = applyEvents(emptyTranscript, events).items.find((entry) => entry.kind === "tool");
  if (item?.kind !== "tool") throw new Error(`no tool item: ${JSON.stringify(events)}`);
  return item;
}

function diffOf(events: WireEvent[]) {
  const item = toolItem(events);
  return toolDiff({ ...item, path: item.summary });
}

/** `+3 text` style rendering of hunks, `.` for an unknown number. */
function render(hunks: readonly Hunk[]): string[][] {
  const sign = { context: " ", added: "+", removed: "-" };
  return hunks.map((hunk) =>
    hunk.map((line: DiffLine) => `${sign[line.kind]}${line.number ?? "."} ${line.text}`),
  );
}

const EDIT_INPUT = {
  file_path: `${CWD}/src/lib.rs`,
  old_string: "fn one() {\n    1\n}",
  new_string: "fn one() -> u8 {\n    1\n}",
};

describe("patch parsing", () => {
  it("numbers lines from the hunk starts and skips no-newline markers", () => {
    const hunks = parsePatch([
      {
        oldStart: 9,
        oldLines: 3,
        newStart: 9,
        newLines: 4,
        lines: [" a", "-b", "+c", "+d", "\\ No newline at end of file", " e"],
      },
    ]);
    expect(hunks === null ? null : render(hunks)).toEqual([
      [" 9 a", "-10 b", "+10 c", "+11 d", " 12 e"],
    ]);
  });

  it("rejects an unexpected shape", () => {
    expect(parsePatch(undefined)).toBeNull();
    expect(parsePatch([{ oldStart: 1, lines: [] }])).toBeNull();
    expect(parsePatch([{ oldStart: 1, newStart: 1, lines: [3] }])).toBeNull();
    expect(parsePatch([])).toEqual([]);
  });
});

describe("line diff", () => {
  it("keeps common lines as context, removed before added", () => {
    expect(render([lineDiff("a\nb\nc\nd", "a\nx\ny\nc\nd\n")])).toEqual([
      [" . a", "-. b", "+. x", "+. y", " . c", " . d"],
    ]);
  });

  it("handles empty sides and CRLF", () => {
    expect(render([lineDiff("", "a\r\nb\r\n")])).toEqual([["+. a", "+. b"]]);
    expect(render([lineDiff("a", "")])).toEqual([["-. a"]]);
    expect(splitLines("a\n\nb\n")).toEqual(["a", "", "b"]);
  });

  it("finds the common lines inside a change", () => {
    expect(render([lineDiff("1\n2\n3\n4", "0\n2\n4\n5")])).toEqual([
      ["-. 1", "+. 0", " . 2", "-. 3", " . 4", "+. 5"],
    ]);
  });
});

describe("tool diffs from transcript fixtures", () => {
  it("Edit with a structuredPatch uses its hunks and numbers", () => {
    const diff = diffOf(
      toolEvents("Edit", EDIT_INPUT, {
        content: "The file has been updated successfully.",
        result: {
          filePath: EDIT_INPUT.file_path,
          oldString: EDIT_INPUT.old_string,
          newString: EDIT_INPUT.new_string,
          originalFile: "fn one() {\n    1\n}\n",
          structuredPatch: [
            {
              oldStart: 1,
              oldLines: 3,
              newStart: 1,
              newLines: 3,
              lines: ["-fn one() {", "+fn one() -> u8 {", "     1", " }"],
            },
          ],
          userModified: false,
          replaceAll: false,
        },
      }),
    );
    expect(diff?.verb).toBe("Update");
    expect(diff?.path).toBe("src/lib.rs");
    expect(diff?.language).toBe("rust");
    expect([diff?.added, diff?.removed]).toEqual([1, 1]);
    expect(render(diff?.hunks ?? [])).toEqual([
      ["-1 fn one() {", "+1 fn one() -> u8 {", " 2     1", " 3 }"],
    ]);
  });

  it("Edit without a result diffs its input, unnumbered, while running", () => {
    const diff = diffOf(toolEvents("Edit", EDIT_INPUT));
    expect(render(diff?.hunks ?? [])).toEqual([
      ["-. fn one() {", "+. fn one() -> u8 {", " .     1", " . }"],
    ]);
    expect(gutterWidth(diff?.hunks ?? [])).toBe(0);
  });

  it("a rejected Edit shows no diff", () => {
    const events = toolEvents("Edit", EDIT_INPUT, {
      content: "The user doesn't want to proceed with this tool use.",
      isError: true,
    });
    expect(diffOf(events)).toBeNull();
    expect(toolItem(events).output).toContain("proceed");
  });

  it("MultiEdit builds one hunk per edit without a patch", () => {
    const diff = diffOf(
      toolEvents(
        "MultiEdit",
        {
          file_path: `${CWD}/web/app.ts`,
          edits: [
            { old_string: "const a = 1;", new_string: "const a = 2;" },
            { old_string: "b()", new_string: "b()\nc()", replace_all: true },
          ],
        },
        { content: "Applied 2 edits." },
      ),
    );
    expect(diff?.language).toBe("typescript");
    expect(render(diff?.hunks ?? [])).toEqual([
      ["-. const a = 1;", "+. const a = 2;"],
      [" . b()", "+. c()"],
    ]);
    expect(changeSummary(diff?.added ?? 0, diff?.removed ?? 0)).toBe(
      "Added 2 lines, removed 1 line",
    );
  });

  it("MultiEdit with a structuredPatch uses it", () => {
    const diff = diffOf(
      toolEvents(
        "MultiEdit",
        { file_path: `${CWD}/a.py`, edits: [{ old_string: "x", new_string: "y" }] },
        {
          content: "Applied 1 edit.",
          result: {
            structuredPatch: [
              { oldStart: 120, oldLines: 1, newStart: 120, newLines: 1, lines: ["-x", "+y"] },
            ],
          },
        },
      ),
    );
    expect(render(diff?.hunks ?? [])).toEqual([["-120 x", "+120 y"]]);
    expect(gutterWidth(diff?.hunks ?? [])).toBe(3);
  });

  it("Write of a new file, as the CLI reports it, adds every line numbered", () => {
    const diff = diffOf(
      toolEvents(
        "Write",
        { file_path: `${CWD}/notes/hello.md`, content: "# Hi\n\nthere\n" },
        {
          content: "File created successfully.",
          result: {
            type: "create",
            filePath: `${CWD}/notes/hello.md`,
            content: "# Hi\n\nthere\n",
            structuredPatch: [],
            originalFile: null,
            userModified: false,
          },
        },
      ),
    );
    expect(diff?.verb).toBe("Write");
    expect(diff?.path).toBe("notes/hello.md");
    expect(render(diff?.hunks ?? [])).toEqual([["+1 # Hi", "+2 ", "+3 there"]]);
    expect(changeSummary(diff?.added ?? 0, diff?.removed ?? 0)).toBe("Added 3 lines");
  });

  it("Write over an existing file uses the result's patch", () => {
    const diff = diffOf(
      toolEvents(
        "Write",
        { file_path: `${CWD}/Cargo.toml`, content: "[package]\nname = \"b\"\n" },
        {
          content: "The file has been updated.",
          result: {
            type: "update",
            filePath: `${CWD}/Cargo.toml`,
            content: "[package]\nname = \"b\"\n",
            structuredPatch: [
              {
                oldStart: 1,
                oldLines: 2,
                newStart: 1,
                newLines: 2,
                lines: [" [package]", "-name = \"a\"", "+name = \"b\""],
              },
            ],
            originalFile: "[package]\nname = \"a\"\n",
          },
        },
      ),
    );
    expect(diff?.verb).toBe("Update");
    expect(diff?.language).toBe("ini");
    expect(render(diff?.hunks ?? [])).toEqual([[" 1 [package]", "-2 name = \"a\"", "+2 name = \"b\""]]);
  });

  it("Write without a result adds its input content", () => {
    const diff = diffOf(toolEvents("Write", { file_path: "/elsewhere/x.txt", content: "a\nb" }));
    expect(diff?.path).toBe("/elsewhere/x.txt");
    expect(diff?.language).toBeNull();
    expect(render(diff?.hunks ?? [])).toEqual([["+1 a", "+2 b"]]);
  });

  it("other tools have no diff", () => {
    expect(diffOf(toolEvents("Read", { file_path: `${CWD}/a.rs` }, { content: "x" }))).toBeNull();
  });
});

describe("summary and highlighting", () => {
  it("words the counts like the TUI", () => {
    expect(changeSummary(1, 0)).toBe("Added 1 line");
    expect(changeSummary(8, 0)).toBe("Added 8 lines");
    expect(changeSummary(0, 2)).toBe("Removed 2 lines");
    expect(changeSummary(0, 0)).toBe("No changes");
  });

  it("highlights each side and maps lines back", () => {
    const hunk = lineDiff("let a = 1;\nlet b = 2;", "let a = 1;\nlet c = \"x\";");
    const html = highlightHunk(hunk, "rust");
    expect(html).toHaveLength(3);
    expect(html[1]).toContain("b");
    expect(html[2]).toContain('<span class="hljs-string">&quot;x&quot;</span>');
    expect(highlightHunk(hunk, null)).toEqual([null, null, null]);
  });
});
