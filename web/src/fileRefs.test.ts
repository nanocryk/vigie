import { describe, expect, it } from "vitest";

import { createKindLookup, parseFileRef, resolveRefPath } from "@/fileRefs";
import type { PathKind } from "@/files";

describe("parseFileRef", () => {
  it("reads absolute and relative paths with a line", () => {
    expect(parseFileRef("web/src/files.ts:42")).toEqual({ path: "web/src/files.ts", line: 42 });
    expect(parseFileRef("C:\\a\\b.rs:7:3")).toEqual({ path: "C:\\a\\b.rs", line: 7 });
    expect(parseFileRef("/a/b.rs:10-20")).toEqual({ path: "/a/b.rs", line: 10 });
    expect(parseFileRef("Cargo.toml")).toEqual({ path: "Cargo.toml", line: null });
    expect(parseFileRef("src/http/")).toEqual({ path: "src/http/", line: null });
    expect(parseFileRef("E:\\my docs\\a.md")).toEqual({ path: "E:\\my docs\\a.md", line: null });
  });

  it("leaves code that is not a path", () => {
    for (const code of [
      "cargo test",
      "useState",
      "x",
      "--flag",
      "a ?? b",
      "https://example.com/a.ts",
      "git add src/a.ts",
      "1.5",
      "C:",
    ]) {
      expect(parseFileRef(code)).toBeNull();
    }
  });

  it("drops a zero line", () => {
    expect(parseFileRef("a.ts:0")).toEqual({ path: "a.ts", line: null });
  });
});

describe("resolveRefPath", () => {
  it("joins relative paths to the cwd with its separator", () => {
    expect(resolveRefPath("web/a.ts", "C:\\p")).toBe("C:\\p\\web\\a.ts");
    expect(resolveRefPath("./a.ts", "/p")).toBe("/p/a.ts");
    expect(resolveRefPath("/x/a.ts", "C:\\p")).toBe("/x/a.ts");
    expect(resolveRefPath("a.ts", null)).toBeNull();
  });
});

describe("createKindLookup", () => {
  const file = (path: string): PathKind => ({ kind: "file", path });

  it("batches lookups of one tick and caches the answers", async () => {
    const requests: string[][] = [];
    const lookup = createKindLookup((paths) => {
      requests.push([...paths]);
      return Promise.resolve(paths.map((path) => (path === "/gone" ? null : file(path))));
    });
    const answers = await Promise.all([lookup("/a"), lookup("/gone"), lookup("/a")]);
    expect(answers).toEqual([file("/a"), null, file("/a")]);
    expect(requests).toEqual([["/a", "/gone"]]);
    expect(await lookup("/a")).toEqual(file("/a"));
    expect(requests).toHaveLength(1);
  });

  it("asks again once a missing answer is stale", async () => {
    let clock = 0;
    let calls = 0;
    const lookup = createKindLookup(
      (paths) => {
        calls += 1;
        return Promise.resolve(paths.map(() => null));
      },
      () => clock,
    );
    await lookup("/gone");
    clock = 10_000;
    await lookup("/gone");
    expect(calls).toBe(1);
    clock = 31_000;
    await lookup("/gone");
    expect(calls).toBe(2);
  });

  it("answers null without caching when the request fails", async () => {
    let calls = 0;
    const lookup = createKindLookup(() => {
      calls += 1;
      return Promise.reject(new Error("offline"));
    });
    expect(await lookup("/a")).toBeNull();
    expect(await lookup("/a")).toBeNull();
    expect(calls).toBe(2);
  });
});
