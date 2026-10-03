// In-app updates. Releases are built by .github/workflows/release.yml when a
// `v*` tag is pushed; the app checks GitHub's latest.json, and the person
// decides when to install (the app restarts to finish).

import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

/** First check shortly after launch, then this often while the app is open. */
export const CHECK_INTERVAL_MS = 4 * 60 * 60 * 1000;
export const FIRST_CHECK_DELAY_MS = 10_000;

export type UpdateState =
  | { kind: "idle" }
  | { kind: "available"; version: string; notes: string }
  | { kind: "downloading"; version: string; percent: number | null }
  | { kind: "restarting" }
  | { kind: "error"; message: string };

/** Whole-number percent, or null when the server sent no size. */
export function progressPercent(
  downloaded: number,
  total: number | undefined,
): number | null {
  if (!total || total <= 0) return null;
  return Math.min(100, Math.floor((downloaded / total) * 100));
}

/** First line of the release notes, trimmed for the sidebar. */
export function shortNotes(body: string | undefined, max = 90): string {
  const line =
    (body ?? "")
      .split("\n")
      .map((l) => l.replace(/^[#*\-\s]+/, "").trim())
      .find(Boolean) ?? "";
  return line.length > max ? line.slice(0, max - 1).trimEnd() + "…" : line;
}

export async function findUpdate(): Promise<Update | null> {
  return check();
}

/** Download, install, restart. `onProgress` gets 0-100 or null when size is unknown. */
export async function installUpdate(
  update: Update,
  onProgress: (percent: number | null) => void,
): Promise<void> {
  let total: number | undefined;
  let downloaded = 0;
  await update.downloadAndInstall((event) => {
    if (event.event === "Started") {
      total = event.data.contentLength;
      onProgress(progressPercent(0, total));
    } else if (event.event === "Progress") {
      downloaded += event.data.chunkLength;
      onProgress(progressPercent(downloaded, total));
    }
  });
  await relaunch();
}
