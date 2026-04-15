import { writable, derived, get } from 'svelte/store';
import { expandedStoryId } from './briefing';
import type { Story } from '$lib/tauri/types';

export const showShortcutHelp = writable(false);
export const storyIds = writable<number[]>([]);
export const focusedIndex = writable(0);

export const focusedStoryId = derived(
	[storyIds, focusedIndex],
	([$ids, $idx]) => $ids[$idx] ?? null
);

export function navigateDown() {
	const ids = get(storyIds);
	focusedIndex.update(i => Math.min(i + 1, ids.length - 1));
}

export function navigateUp() {
	focusedIndex.update(i => Math.max(i - 1, 0));
}

export function expandFocused() {
	const id = get(focusedStoryId);
	if (id !== null) {
		expandedStoryId.set(id);
	}
}

export function updateStoryList(stories: Story[]) {
	storyIds.set(stories.map(s => s.id));
	focusedIndex.set(0);
}
