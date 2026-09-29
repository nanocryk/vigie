import { describe, expect, it } from "vitest";

import { attachmentName, composeText, isImagePath, uploadsSession } from "@/attachments";

describe("attachments", () => {
  // `session::attachments::tests::compose_appends_the_block` checks the same strings.
  it("compose the text Claude gets as the server does", () => {
    expect(composeText("hi", [])).toBe("hi");
    const files = ["C:\\u\\a.png", "/tmp/b.txt"];
    expect(composeText("look", files)).toBe("look\n\nAttachments:\n- C:\\u\\a.png\n- /tmp/b.txt");
    expect(composeText(" \n", files.slice(0, 1))).toBe("Attachments:\n- C:\\u\\a.png");
  });

  it("are named without the upload prefix", () => {
    expect(attachmentName("C:\\h\\.vigie2\\uploads\\s\\0a1b2c3d4e5f-shot.png")).toBe("shot.png");
    expect(attachmentName("/tmp/notes-v2.md")).toBe("notes-v2.md");
  });

  it("name the session of an uploads folder", () => {
    const id = "fce5a5d9-b622-4a23-8410-a8d5e4f2ba87";
    expect(uploadsSession(`C:\\Users\\u\\.vigie2\\uploads\\${id}\\ab-shot.png`)).toBe(id);
    expect(uploadsSession(`/home/u/.vigie2/uploads/${id}`)).toBe(id);
    expect(uploadsSession("C:\\Users\\u\\.vigie2\\uploads")).toBe(null);
    expect(uploadsSession(`C:\\work\\uploads\\${id}\\a.png`)).toBe(null);
  });

  it("tell images by extension", () => {
    expect(isImagePath("a/B.JPG")).toBe(true);
    expect(isImagePath("a/b.txt")).toBe(false);
  });
});
