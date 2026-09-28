import { describe, expect, it } from "vitest";

import { folderSegments, offerSegments } from "@/grant";

describe("folderSegments", () => {
  it("splits a Windows path into its folders, root first", () => {
    expect(folderSegments("C:\\Users\\me\\Temp")).toEqual([
      { separator: "", label: "C:", path: "C:\\" },
      { separator: "\\", label: "Users", path: "C:\\Users" },
      { separator: "\\", label: "me", path: "C:\\Users\\me" },
      { separator: "\\", label: "Temp", path: "C:\\Users\\me\\Temp" },
    ]);
  });

  it("reads forward slashes, a lowercase drive and stray separators", () => {
    expect(folderSegments("c:/a//b/").map((segment) => segment.path)).toEqual([
      "C:\\",
      "C:\\a",
      "C:\\a\\b",
    ]);
    expect(folderSegments("D:\\")).toEqual([{ separator: "", label: "D:", path: "D:\\" }]);
    expect(folderSegments("E:").map((segment) => segment.path)).toEqual(["E:\\"]);
  });

  it("keeps spaces and dots inside a folder name", () => {
    expect(folderSegments("E:\\vigie workspace\\v2.0").map((segment) => segment.label)).toEqual([
      "E:",
      "vigie workspace",
      "v2.0",
    ]);
  });

  it("splits a POSIX path without doubling the root slash", () => {
    expect(folderSegments("/home/me")).toEqual([
      { separator: "", label: "/", path: "/" },
      { separator: "", label: "home", path: "/home" },
      { separator: "/", label: "me", path: "/home/me" },
    ]);
  });

  it("has no segments for a relative path", () => {
    expect(folderSegments("src\\lib")).toEqual([]);
    expect(folderSegments("C:relative")).toEqual([]);
    expect(folderSegments("")).toEqual([]);
  });
});

describe("offerSegments", () => {
  it("flags the folders the server lists as broad, ignoring case", () => {
    const segments = offerSegments({
      kind: "read",
      folder: "C:\\Users\\me\\AppData\\Local\\Temp\\x",
      broad: ["C:\\", "c:\\users", "C:\\Users\\me", "C:\\Users\\me\\AppData"],
    });
    expect(segments.map((segment) => [segment.label, segment.broad])).toEqual([
      ["C:", true],
      ["Users", true],
      ["me", true],
      ["AppData", true],
      ["Local", false],
      ["Temp", false],
      ["x", false],
    ]);
  });
});
