import { describe, expect, it } from "vitest";

import {
  type ChangedFile,
  absolutePath,
  blockDiff,
  folderMarks,
  groupByFolder,
  gutterMarks,
  parseRepoDiff,
  startsCollapsed,
} from "@/git";

function file(path: string, added = 1, removed = 0): ChangedFile {
  return { path, oldPath: null, status: "modified", added, removed, note: null, hunks: [] };
}

describe("parseRepoDiff", () => {
  it("maps the server fields and names a bad one", () => {
    const diff = parseRepoDiff(
      {
        root: "C:\\p",
        files: [
          {
            path: "a.rs",
            old_path: null,
            status: "modified",
            added: 1,
            removed: 1,
            note: null,
            hunks: [
              {
                lines: [
                  { kind: "removed", text: "x", old: 3, new: null },
                  { kind: "added", text: "y", old: null, new: 3 },
                ],
              },
            ],
          },
        ],
      },
      "diff",
    );
    expect(diff.files[0]?.hunks[0]?.map((line) => line.kind)).toEqual(["removed", "added"]);
    expect(() =>
      parseRepoDiff({ root: "r", files: [{ ...file("a"), status: "moved" }] }, "diff"),
    ).toThrow(/diff\.files\[0\]\.status/);
  });
});

describe("marks", () => {
  const line = (
    kind: "context" | "added" | "removed",
    old: number | null,
    next: number | null,
  ) => ({
    kind,
    text: "",
    old,
    new: next,
  });

  it("marks added, modified and removed-above lines", () => {
    const changed: ChangedFile = {
      ...file("a.ts"),
      hunks: [
        [
          line("context", 1, 1),
          line("added", null, 2),
          line("context", 2, 3),
          line("removed", 3, null),
          line("added", null, 4),
          line("context", 4, 5),
          line("removed", 5, null),
          line("removed", 6, null),
          line("context", 7, 6),
          line("removed", 8, null),
        ],
      ],
    };
    const marks = gutterMarks(changed);
    expect([...marks.added]).toEqual([2]);
    expect([...marks.modified]).toEqual([4]);
    expect([...marks.removedAbove]).toEqual([6, 7]);
  });

  it("marks folder rows and collects deletions directly inside", () => {
    const status = {
      root: "C:\\repo",
      changes: [
        { path: "src/a.ts", kind: "modified" as const },
        { path: "src/new/x.ts", kind: "added" as const },
        { path: "src/new/y.ts", kind: "added" as const },
        { path: "src/old/z.ts", kind: "deleted" as const },
        { path: "src/gone.ts", kind: "deleted" as const },
        { path: "README.md", kind: "added" as const },
      ],
    };
    const marks = folderMarks(status, "C:\\repo\\src");
    expect(Object.fromEntries(marks.rows)).toEqual({
      "a.ts": "modified",
      new: "added",
      old: "modified",
    });
    expect(marks.deleted).toEqual(["src/gone.ts"]);
    expect(Object.fromEntries(folderMarks(status, "C:\\repo").rows)).toEqual({
      src: "modified",
      "README.md": "added",
    });
  });
});

describe("helpers", () => {
  it("collapses long diffs and lockfiles", () => {
    expect(startsCollapsed(file("src/a.ts", 150, 60))).toBe(true);
    expect(startsCollapsed(file("web/package-lock.json", 3))).toBe(true);
    expect(startsCollapsed(file("Cargo.lock", 3))).toBe(true);
    expect(startsCollapsed(file("src/a.ts", 10, 5))).toBe(false);
  });

  it("groups by folder with the root first", () => {
    const groups = groupByFolder([
      file("z/b.ts"),
      file("README.md"),
      file("a/x.ts"),
      file("z/a.ts"),
    ]);
    expect(groups.map((group) => [group.folder, group.files.map((f) => f.path)])).toEqual([
      ["", ["README.md"]],
      ["a", ["a/x.ts"]],
      ["z", ["z/b.ts", "z/a.ts"]],
    ]);
  });

  it("numbers removed lines by their old number", () => {
    const changed: ChangedFile = {
      ...file("a.rs"),
      hunks: [
        [
          { kind: "removed", text: "x", old: 7, new: null },
          { kind: "added", text: "y", old: null, new: 8 },
        ],
      ],
    };
    expect(blockDiff(changed).hunks[0]?.map((line) => line.number)).toEqual([7, 8]);
  });

  it("joins a relative path in the root's own form", () => {
    expect(absolutePath("C:\\work\\app", "src/main.rs")).toBe("C:\\work\\app\\src\\main.rs");
    expect(absolutePath("/srv/app", "src/main.rs")).toBe("/srv/app/src/main.rs");
  });
});
