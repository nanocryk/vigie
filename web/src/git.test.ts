import { describe, expect, it } from "vitest";

import {
  type ChangedFile,
  absolutePath,
  blockDiff,
  groupByFolder,
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

describe("helpers", () => {
  it("collapses long diffs and lockfiles", () => {
    expect(startsCollapsed(file("src/a.ts", 150, 60))).toBe(true);
    expect(startsCollapsed(file("web/package-lock.json", 3))).toBe(true);
    expect(startsCollapsed(file("Cargo.lock", 3))).toBe(true);
    expect(startsCollapsed(file("src/a.ts", 10, 5))).toBe(false);
  });

  it("groups by folder with the root first", () => {
    const groups = groupByFolder([file("z/b.ts"), file("README.md"), file("a/x.ts"), file("z/a.ts")]);
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
