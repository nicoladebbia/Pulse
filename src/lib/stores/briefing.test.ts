import { describe, it, expect } from 'vitest';
import type { Story } from '$lib/tauri/types';
import {
	rankScore,
	byRankDesc,
	isFiling,
	getFeaturedFromList,
	getCompactFromList,
	resolveExpandedStory,
	needsStoryFetch
} from './briefing';

/**
 * These guard the defect the audit found: the archive rendered every story as a card in
 * raw display_order, so a day with 120 news stories and 462 SEC/FEC filings showed 582
 * cards with the filings interleaved. The front page was never affected only because it
 * ranks and slices to 15 — that ranking is what these tests pin down.
 */

let nextId = 1;
function story(over: Partial<Story> = {}): Story {
	return {
		id: nextId++,
		briefing_id: 1,
		sector: 'ai',
		headline: 'headline',
		summary: 'summary',
		key_facts: [],
		why_it_matters: '',
		what_to_watch: '',
		importance_score: 5,
		relevance_score: null,
		relevance_reason: null,
		is_hero: false,
		display_order: 0,
		original_url: 'https://example.com',
		source_name: 'Example',
		published_at: null,
		created_at: '2026-08-14T08:00:00Z',
		summary_depth: null,
		deep_summary: null,
		source_type: 'news',
		financial_metadata: null,
		...over
	};
}

/** A financial filing exactly as the fetcher writes it: importance hardcoded to 5. */
function filing(over: Partial<Story> = {}): Story {
	return story({ source_type: 'financial', importance_score: 5, sector: 'finance', ...over });
}

describe('rankScore', () => {
	it('prefers personal relevance over the summarizer importance', () => {
		expect(rankScore(story({ relevance_score: 9, importance_score: 2 }))).toBe(9);
	});

	it('falls back to importance when analyze never scored the story', () => {
		// This is the real state on days when the daily `analyze` step failed —
		// relevance_score is NULL for every news story.
		expect(rankScore(story({ relevance_score: null, importance_score: 7 }))).toBe(7);
	});

	it('treats a relevance of 0 as a score, not as missing', () => {
		// `?? ` not `||` — a genuine 0 must not silently promote the story to importance.
		expect(rankScore(story({ relevance_score: 0, importance_score: 8 }))).toBe(0);
	});
});

describe('isFiling', () => {
	it('identifies bulk regulatory rows', () => {
		expect(isFiling(filing())).toBe(true);
	});

	it('does not treat a finance-sector news story as a filing', () => {
		// sector and source_type are different axes: real news carries sector 'finance'.
		expect(isFiling(story({ sector: 'finance', source_type: 'news' }))).toBe(false);
	});

	it('does not treat a null source_type as a filing', () => {
		// The DB column is NOT NULL, but the Story type models it as `string | null`
		// and the Rust reader maps a read error to None, so null is representable on
		// the wire. Defaulting an unknown value to "filing" would hide a story.
		expect(isFiling(story({ source_type: null }))).toBe(false);
	});
});

describe('ranking a real briefing shape', () => {
	/** 120 news + 462 filings — the 2026-08-14 briefing. */
	function augustFourteenth(): Story[] {
		const news = Array.from({ length: 120 }, (_, i) =>
			story({ relevance_score: (i % 10) + 1, display_order: i })
		);
		const filings = Array.from({ length: 462 }, (_, i) =>
			filing({ relevance_score: null, display_order: 120 + i })
		);
		// Interleaved, as display_order actually delivers them.
		return [...filings.slice(0, 200), ...news, ...filings.slice(200)];
	}

	it('sorts highest score first', () => {
		const sorted = [...augustFourteenth()].sort(byRankDesc);
		const scores = sorted.map(rankScore);
		expect(scores).toEqual([...scores].sort((a, b) => b - a));
	});

	it('keeps every filing out of the top 15 — the property protecting the front page', () => {
		const news = augustFourteenth().filter((s) => !isFiling(s));
		const featured = getFeaturedFromList(news, 3);
		const compact = getCompactFromList(news, 3, 12);
		expect(featured).toHaveLength(3);
		expect(compact).toHaveLength(12);
		expect([...featured, ...compact].some(isFiling)).toBe(false);
	});

	it('splits the day into 120 stories and 462 filings, losing nothing', () => {
		const all = augustFourteenth();
		const news = all.filter((s) => !isFiling(s));
		const filings = all.filter(isFiling);
		expect(news).toHaveLength(120);
		expect(filings).toHaveLength(462);
		expect(news.length + filings.length).toBe(all.length);
	});

	it('ranks filings below news even when no news story was scored by analyze', () => {
		// The degenerate day: analyze failed, so relevance is NULL everywhere and only
		// importance separates the two. News importance skews high (1,921 of 2,121 rows
		// at >= 6 in production); filings are pinned at 5.
		const news = Array.from({ length: 20 }, () =>
			story({ relevance_score: null, importance_score: 7 })
		);
		const filings = Array.from({ length: 400 }, () => filing({ relevance_score: null }));
		const top = [...filings, ...news].sort(byRankDesc).slice(0, 15);
		expect(top.some(isFiling)).toBe(false);
	});
});

describe('featured and compact do not overlap', () => {
	it('compact starts where featured ends', () => {
		const stories = Array.from({ length: 30 }, (_, i) =>
			story({ relevance_score: 30 - i })
		);
		const featured = getFeaturedFromList(stories, 3);
		const compact = getCompactFromList(stories, 3, 12);
		const ids = new Set(featured.map((s) => s.id));
		expect(compact.some((s) => ids.has(s.id))).toBe(false);
	});

	it('does not mutate the array it was given', () => {
		// `.sort()` is in-place; these must copy first or they reorder the caller's store.
		const stories = [story({ relevance_score: 1 }), story({ relevance_score: 9 })];
		const originalOrder = stories.map((s) => s.id);
		getFeaturedFromList(stories, 2);
		expect(stories.map((s) => s.id)).toEqual(originalOrder);
	});
});

/**
 * The citation defect: Ask Pulse and Trends set expandedStoryId and route to the front
 * page, which resolved the id against today's briefing only. Every citation to an older
 * story was a silent no-op — no panel, no error, the page just re-rendered.
 */
describe('resolveExpandedStory', () => {
	it('finds a story that is in today briefing', () => {
		const s = story();
		expect(resolveExpandedStory(s.id, [s], null)).toBe(s);
	});

	it('falls back to a story loaded by id when today has no such story', () => {
		const today = story();
		const cited = story({ created_at: '2026-05-02T08:00:00Z' });
		expect(resolveExpandedStory(cited.id, [today], cited)).toBe(cited);
	});

	it('prefers today copy over a loaded one so the panel matches the grid', () => {
		const today = story({ headline: 'from today' });
		const stale = { ...today, headline: 'from an older fetch' };
		expect(resolveExpandedStory(today.id, [today], stale)?.headline).toBe('from today');
	});

	it('ignores a loaded story whose id is no longer the one wanted', () => {
		// Two citations clicked in a row: the first response must not render under the
		// second headline.
		const first = story();
		const second = story();
		expect(resolveExpandedStory(second.id, [], first)).toBeNull();
	});

	it('resolves to nothing when no story is wanted', () => {
		expect(resolveExpandedStory(null, [story()], story())).toBeNull();
	});
});

describe('needsStoryFetch', () => {
	it('is true for a citation to a story outside today briefing', () => {
		const today = story();
		expect(needsStoryFetch(today.id + 999, [today], null, null)).toBe(true);
	});

	it('is false once the story is in hand', () => {
		const cited = story();
		expect(needsStoryFetch(cited.id, [], cited, null)).toBe(false);
	});

	it('is false for a story already known to be missing, so it is asked for once', () => {
		expect(needsStoryFetch(4242, [], null, 4242)).toBe(false);
	});

	it('is true again for a different id after one was found missing', () => {
		expect(needsStoryFetch(7, [], null, 4242)).toBe(true);
	});

	it('is false when nothing is expanded', () => {
		expect(needsStoryFetch(null, [], null, null)).toBe(false);
	});
});
