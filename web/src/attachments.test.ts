import { describe, expect, it } from "vitest";

import { attachmentName, composeText, isImagePath } from "@/attachments";

describe("attachments", () => {
  // `session::attachments::tests::compose_appends_the_block` checks the same strings.
  it("compose the text Claude gets as the server does", () => {
    expect(composeText("hi", [])).toBe("hi");
    const files = ["C:\\u\\a.png", "/tmp/b.txt"];
    expect(composeText("look", files)).toBe("look\n\nAttachments:\n- C:\\u\\a.png\n- /tmp/b.txt");
    expect(composeText(" \n", files.slice(0, 1))).toBe("Attachments:\n- C:\\u\\a.png");
  });

  it("are named without the upload prefix", () => {
    expect(attachmentName("C:\\h\\.vigie\\uploads\\s\\0a1b2c3d4e5f-shot.png")).toBe("shot.png");
    expect(attachmentName("/tmp/notes-v2.md")).toBe("notes-v2.md");
  });

  it("tell images by extension", () => {
    expect(isImagePath("a/B.JPG")).toBe(true);
    expect(isImagePath("a/b.txt")).toBe(false);
  });
});
