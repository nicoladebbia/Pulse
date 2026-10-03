import { describe, it, expect, vi } from "vitest";

vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn() }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn() }));

import { progressPercent, shortNotes, installUpdate } from "./updater";
import { relaunch } from "@tauri-apps/plugin-process";

describe("progressPercent", () => {
  it("rounds down and caps at 100", () => {
    expect(progressPercent(0, 1000)).toBe(0);
    expect(progressPercent(999, 1000)).toBe(99);
    expect(progressPercent(1200, 1000)).toBe(100);
  });

  it("is null when the size is unknown", () => {
    expect(progressPercent(500, undefined)).toBeNull();
    expect(progressPercent(500, 0)).toBeNull();
  });
});

describe("shortNotes", () => {
  it("takes the first non-empty line without markdown bullets or headings", () => {
    expect(shortNotes("\n## What's new\n- faster search")).toBe("What's new");
    expect(shortNotes("- Local AI mode\n- fixes")).toBe("Local AI mode");
  });

  it("trims long lines and handles missing notes", () => {
    expect(shortNotes("x".repeat(200), 10)).toBe("xxxxxxxxx…");
    expect(shortNotes(undefined)).toBe("");
  });
});

describe("installUpdate", () => {
  it("reports progress from the download events, then restarts", async () => {
    const seen: (number | null)[] = [];
    const update = {
      downloadAndInstall: async (cb: (e: unknown) => void) => {
        cb({ event: "Started", data: { contentLength: 200 } });
        cb({ event: "Progress", data: { chunkLength: 50 } });
        cb({ event: "Progress", data: { chunkLength: 150 } });
        cb({ event: "Finished" });
      },
    };
    await installUpdate(update as never, (p) => seen.push(p));
    expect(seen).toEqual([0, 25, 100]);
    expect(relaunch).toHaveBeenCalledOnce();
  });

  it("does not restart when the download fails", async () => {
    vi.mocked(relaunch).mockClear();
    const update = {
      downloadAndInstall: async () => {
        throw new Error("offline");
      },
    };
    await expect(installUpdate(update as never, () => {})).rejects.toThrow(
      "offline",
    );
    expect(relaunch).not.toHaveBeenCalled();
  });
});
