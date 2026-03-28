import { writable } from 'svelte/store';
import type { BriefingWithStories, Story } from '$lib/tauri/types';

export const currentBriefing = writable<BriefingWithStories | null>(null);
export const isLoading = writable(true);
export const activeSector = writable<string | null>(null);
export const expandedStoryId = writable<number | null>(null);

export function getFilteredStories(briefing: BriefingWithStories | null, sector: string | null): Story[] {
	if (!briefing) return [];
	if (!sector) return briefing.stories;
	return briefing.stories.filter(s => s.sector === sector);
}
