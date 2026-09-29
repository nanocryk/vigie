import { describe, expect, it } from "vitest";

import {
  type ExplorerState,
  activeTab,
  canGoUp,
  closeTab,
  goUp,
  crumbs,
  initialState,
  isInside,
  joinPath,
  newFreeTab,
  openSessionTab,
  parseFileView,
  parseListing,
  parseStoredState,
  pruneSessionTabs,
  selectLastFree,
  selectTab,
  updateTab,
  upSteps,
} from "@/files";

describe("paths", () => {
  it("splits a Windows path into crumbs", () => {
    expect(crumbs("C:\\Users\\me")).toEqual([
      { label: "C:", path: "C:\\" },
      { label: "Users", path: "C:\\Users" },
      { label: "me", path: "C:\\Users\\me" },
    ]);
    expect(crumbs("D:\\")).toEqual([{ label: "D:", path: "D:\\" }]);
  });

  it("splits UNC and POSIX paths into crumbs", () => {
    expect(crumbs("\\\\nas\\share\\docs").map((crumb) => crumb.path)).toEqual([
      "\\\\nas\\share\\",
      "\\\\nas\\share\\docs",
    ]);
    expect(crumbs("/home/me").map((crumb) => crumb.path)).toEqual(["/", "/home", "/home/me"]);
  });

  it("joins with the path's own separator", () => {
    expect(joinPath("C:\\", "a")).toBe("C:\\a");
    expect(joinPath("C:\\a", "b")).toBe("C:\\a\\b");
    expect(joinPath("/", "a")).toBe("/a");
    expect(joinPath("/a", "b")).toBe("/a/b");
  });

  it("tells whether a path is inside a root", () => {
    expect(isInside("C:\\Work\\app\\src", "c:\\work\\app")).toBe(true);
    expect(isInside("C:\\work\\app", "C:\\work\\app\\")).toBe(true);
    expect(isInside("C:\\work\\apple", "C:\\work\\app")).toBe(false);
    expect(isInside("/srv/App", "/srv/app")).toBe(false);
  });
});

describe("parsers", () => {
  it("parses a listing", () => {
    const listing = parseListing(
      {
        path: "C:\\p",
        parent: "C:\\",
        entries: [
          { name: "src", kind: "dir", size: null, modified: null, hidden: false },
          { name: "a.md", kind: "file", size: 3, modified: "2026-09-29T10:00:00Z", hidden: false },
        ],
      },
      "listing",
    );
    expect(listing.entries.map((entry) => entry.size)).toEqual([null, 3]);
  });

  it("parses each file kind and names a bad one", () => {
    const base = { path: "p", size: 1, modified: null };
    expect(parseFileView({ ...base, kind: "text", text: "x", truncated: false }, "f").kind).toBe(
      "text",
    );
    expect(parseFileView({ ...base, kind: "image" }, "f").kind).toBe("image");
    expect(() => parseFileView({ ...base, kind: "video" }, "f")).toThrow(/f\.kind/);
  });
});

describe("tabs", () => {
  const withTwoFree = (): ExplorerState => newFreeTab(initialState());

  it("keeps the last free tab open", () => {
    const state = initialState();
    expect(closeTab(state, state.active)).toBe(state);
    const two = withTwoFree();
    expect(closeTab(two, two.active).tabs).toHaveLength(1);
  });

  it("opens the free tab used last from Home, even after a session tab", () => {
    let state = withTwoFree();
    const second = state.active;
    state = openSessionTab(state, "s1", "C:\\p");
    expect(activeTab(state).owner).toEqual({ kind: "session", session: "s1", root: "C:\\p" });
    expect(activeTab(state).folder).toBe("C:\\p");
    state = selectLastFree(state);
    expect(state.active).toBe(second);
  });

  it("reuses a session's tab where it was left", () => {
    let state = openSessionTab(initialState(), "s1", "C:\\p");
    const id = state.active;
    state = updateTab(state, id, { folder: "C:\\other" });
    state = selectTab(state, state.tabs[0]?.id ?? "");
    state = openSessionTab(state, "s1", "C:\\p");
    expect(state.active).toBe(id);
    expect(activeTab(state).folder).toBe("C:\\other");
  });

  it("drops the tabs of sessions no longer live", () => {
    let state = openSessionTab(initialState(), "s1", "C:\\a");
    state = openSessionTab(state, "s2", "C:\\b");
    state = pruneSessionTabs(state, new Set(["s2"]));
    expect(state.tabs.map((tab) => tab.owner.kind)).toEqual(["free", "session"]);
    expect(activeTab(state).owner).toMatchObject({ session: "s2" });
  });

  it("goes up from a file, then folder by folder, then to Places", () => {
    const start = initialState();
    let state = updateTab(start, start.active, { folder: "C:\\a\\b", file: "C:\\a\\b\\f.txt" });
    const steps: (string | null)[] = [];
    expect(upSteps(state)).toBe(4);
    while (canGoUp(state)) {
      state = goUp(state);
      steps.push(activeTab(state).file ?? activeTab(state).folder);
    }
    expect(steps).toEqual(["C:\\a\\b", "C:\\a", "C:\\", null]);
  });

  it("round-trips through storage and recovers from garbage", () => {
    const state = openSessionTab(updateTab(withTwoFree(), "x", {}), "s1", "/p");
    expect(parseStoredState(JSON.stringify(state))).toEqual(state);
    expect(parseStoredState("{").tabs).toHaveLength(1);
    expect(parseStoredState(JSON.stringify({ tabs: [] })).tabs).toHaveLength(1);
  });
});
