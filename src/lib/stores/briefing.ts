import { writable } from 'svelte/store';
import type { BriefingWithStories, Story } from '$lib/tauri/types';

export const currentBriefing = writable<BriefingWithStories | null>(null);
export const isLoading = writable(true);
export const activeSectors = writable<string[]>([]); // empty = all sectors
export const expandedStoryId = writable<number | null>(null);

export function getFilteredStories(briefing: BriefingWithStories | null, sectors: string[]): Story[] {
	if (!briefing) return [];
	if (sectors.length === 0) return briefing.stories;
	return briefing.stories.filter(s => sectors.includes(s.sector));
}

/** Sort stories by relevance (personal) then importance, return top N. */
export function getFeaturedFromList(stories: Story[], count: number = 3): Story[] {
	return [...stories]
		.sort((a, b) => {
			const scoreA = a.relevance_score ?? a.importance_score;
			const scoreB = b.relevance_score ?? b.importance_score;
			return scoreB - scoreA;
		})
		.slice(0, count);
}

/** Get the next N stories after skipping the featured ones. */
export function getCompactFromList(stories: Story[], skip: number = 3, count: number = 12): Story[] {
	return [...stories]
		.sort((a, b) => {
			const scoreA = a.relevance_score ?? a.importance_score;
			const scoreB = b.relevance_score ?? b.importance_score;
			return scoreB - scoreA;
		})
		.slice(skip, skip + count);
}
