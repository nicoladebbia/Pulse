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

/**
 * A story's display rank: personal relevance when the daily `analyze` step scored it,
 * otherwise the summarizer's importance. This is the only thing keeping bulk financial
 * filings (hardcoded importance 5) off the front page, so every ranked surface must use
 * it — the archive grid did not, and rendered hundreds of filings in raw insert order.
 */
export function rankScore(s: Story): number {
	return s.relevance_score ?? s.importance_score;
}

/** Highest-ranked first. */
export function byRankDesc(a: Story, b: Story): number {
	return rankScore(b) - rankScore(a);
}

/** Sort stories by relevance (personal) then importance, return top N. */
export function getFeaturedFromList(stories: Story[], count: number = 3): Story[] {
	return [...stories].sort(byRankDesc).slice(0, count);
}

/** Get the next N stories after skipping the featured ones. */
export function getCompactFromList(stories: Story[], skip: number = 3, count: number = 12): Story[] {
	return [...stories].sort(byRankDesc).slice(skip, skip + count);
}

/** True for bulk regulatory/financial filings, which are reference material, not stories. */
export function isFiling(s: Story): boolean {
	return s.source_type === 'financial';
}

/**
 * Resolve the story the expanded panel should render.
 *
 * Ask Pulse cites from the whole corpus and Trends spans months, so the cited id is
 * usually NOT in today's briefing. Matching against today alone returned null, and the
 * front page then rendered as though the citation had never been clicked. `loaded` is a
 * story fetched by id; it only counts when it is the one currently wanted, so a response
 * that arrives after the user has clicked a second citation cannot be shown under the
 * wrong headline.
 */
export function resolveExpandedStory(
	wantedId: number | null,
	todayStories: Story[],
	loaded: Story | null
): Story | null {
	if (wantedId === null) return null;
	return (
		todayStories.find(s => s.id === wantedId) ??
		(loaded && loaded.id === wantedId ? loaded : null)
	);
}

/**
 * Whether the panel still needs to load the wanted story. False once it is in hand and
 * false once it is known missing — otherwise a story that has been pruned from the DB
 * would be re-requested on every re-render.
 */
export function needsStoryFetch(
	wantedId: number | null,
	todayStories: Story[],
	loaded: Story | null,
	knownMissingId: number | null
): boolean {
	if (wantedId === null || wantedId === knownMissingId) return false;
	return resolveExpandedStory(wantedId, todayStories, loaded) === null;
}
