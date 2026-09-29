/**
 * Explicit curation feedback: a one-tap "mattered" / "didn't" per story.
 *
 * This is the ground truth a ranker is scored against, so two properties matter:
 *  - A tap must never be lost silently. The label is applied optimistically and
 *    reverted if the write fails, so what the control shows is what is stored.
 *  - Showing a briefing must not cost 120 round trips. Each control asks for its
 *    story's label; the asks made in one tick are flushed as a single lookup.
 */

import { writable, get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { isTauri } from '$lib/tauri/mock';

export type FeedbackLabel = 'mattered' | 'didnt';

/** story id -> label, for every story whose label has been looked up or set. */
export const feedbackLabels = writable<Record<number, FeedbackLabel>>({});

type Invoker = (cmd: string, args: Record<string, unknown>) => Promise<unknown>;

let invoker: Invoker = (cmd, args) => invoke(cmd, args);
let inApp: () => boolean = () => typeof window !== 'undefined' && isTauri();

/** Tests swap the Tauri bridge for a fake; the app never calls this. */
export function _setBridge(fn: Invoker, app: () => boolean = () => true): void {
	invoker = fn;
	inApp = app;
	requested.clear();
	pending.clear();
	scheduled = false;
	feedbackLabels.set({});
}

/** Tapping the active label clears it; tapping the other one switches. */
export function nextLabel(current: FeedbackLabel | null, tapped: FeedbackLabel): FeedbackLabel | null {
	return current === tapped ? null : tapped;
}

function apply(storyId: number, label: FeedbackLabel | null): void {
	feedbackLabels.update((m) => {
		const next = { ...m };
		if (label === null) delete next[storyId];
		else next[storyId] = label;
		return next;
	});
}

export async function toggleFeedback(storyId: number, tapped: FeedbackLabel): Promise<void> {
	const prev = get(feedbackLabels)[storyId] ?? null;
	const next = nextLabel(prev, tapped);
	apply(storyId, next);
	// Browser dev mode has no backend; the control still works locally.
	if (!inApp()) return;
	try {
		await invoker('set_story_feedback', { storyId, label: next });
	} catch (err) {
		console.warn('[feedback] not saved, reverting', err);
		apply(storyId, prev);
	}
}

const requested = new Set<number>();
const pending = new Set<number>();
let scheduled = false;

async function flush(): Promise<void> {
	scheduled = false;
	const ids = [...pending];
	pending.clear();
	if (ids.length === 0 || !inApp()) return;
	try {
		const rows = (await invoker('get_story_feedback', { storyIds: ids })) as {
			story_id: number;
			label: FeedbackLabel;
		}[];
		feedbackLabels.update((m) => {
			const next = { ...m };
			for (const r of rows) {
				// A tap that landed while the lookup was in flight wins over the stored value.
				if (!(r.story_id in m)) next[r.story_id] = r.label;
			}
			return next;
		});
	} catch (err) {
		// Unlabelled-looking controls are the safe failure: a tap still writes.
		console.warn('[feedback] lookup failed', err);
		for (const id of ids) requested.delete(id);
	}
}

/** Ask for a story's stored label. Asks within one tick share a single lookup. */
export function requestFeedback(storyId: number): void {
	if (requested.has(storyId)) return;
	requested.add(storyId);
	pending.add(storyId);
	if (!scheduled) {
		scheduled = true;
		queueMicrotask(() => void flush());
	}
}
