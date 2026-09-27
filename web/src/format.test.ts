import { describe, expect, it } from "vitest";

import { formatDuration, formatExact, formatStamp } from "@/format";

// Local times, so the cases hold in any time zone. 2026-09-29 is a Tuesday.
const local = (month: number, day: number, hour: number, minute: number, year = 2026) =>
  new Date(year, month - 1, day, hour, minute).getTime();
const NOW = local(9, 29, 10, 30);

describe("formatStamp", () => {
  it("shows only the clock the same day", () => {
    expect(formatStamp(local(9, 29, 8, 5), NOW)).toBe("08:05");
    expect(formatStamp(local(9, 29, 0, 0), NOW)).toBe("00:00");
  });

  it("adds the weekday within the last week", () => {
    expect(formatStamp(local(9, 28, 23, 59), NOW)).toBe("Mon 23:59");
    expect(formatStamp(local(9, 23, 14, 0), NOW)).toBe("Wed 14:00");
  });

  it("uses the date from a week on", () => {
    expect(formatStamp(local(9, 22, 7, 0), NOW)).toBe("Sep 22 07:00");
    expect(formatStamp(local(9, 21, 9, 15), NOW)).toBe("Sep 21 09:15");
  });

  it("adds the year when not the current one", () => {
    expect(formatStamp(local(5, 12, 16, 40, 2025), NOW)).toBe("May 12 2025 16:40");
  });

  it("switches at local midnight, not after 24 hours", () => {
    const justAfterMidnight = local(9, 29, 0, 1);
    expect(formatStamp(local(9, 28, 23, 59), justAfterMidnight)).toBe("Mon 23:59");
    expect(formatStamp(local(9, 29, 0, 0), justAfterMidnight)).toBe("00:00");
  });

  it("treats a time ahead of now as today", () => {
    expect(formatStamp(local(9, 29, 10, 31), NOW)).toBe("10:31");
  });
});

describe("formatExact and formatDuration", () => {
  it("give seconds and a readable duration", () => {
    expect(formatExact(new Date(2026, 8, 29, 14, 5, 32).getTime())).toBe("14:05:32");
    expect(formatDuration(350)).toBe("350 ms");
    expect(formatDuration(2400)).toBe("2.4 s");
    expect(formatDuration(65_000)).toBe("1 min 5 s");
  });
});
