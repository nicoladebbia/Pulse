import { describe, expect, it } from 'vitest';
import { decisionLabel, groupRuns, skipCounts } from './trade-decisions';
import type { TradeDecision } from '$lib/tauri/types';

const d = (run_at: string, ticker: string, outcome: string, reason: string): TradeDecision => ({
	run_at, ticker, name: null, score: 0.4, outcome, reason, detail: null, direction: 'long',
});

describe('trade decisions', () => {
	const rows = [
		d('2026-10-06T13:00:01', 'AAA', 'skipped', 'too_calm'),
		d('2026-10-06T13:00:01', 'BBB', 'bought', 'bought'),
		d('2026-10-06T10:00:02', 'CCC', 'skipped', 'too_calm'),
		d('2026-10-06T10:00:02', 'DDD', 'skipped', 'earnings'),
	];

	it('groups by run, newest first', () => {
		const runs = groupRuns(rows);
		expect(runs.map((r) => r.at)).toEqual(['2026-10-06T13:00:01', '2026-10-06T10:00:02']);
		expect(runs[0].rows.map((r) => r.ticker)).toEqual(['AAA', 'BBB']);
	});

	it('counts only skips, most common first', () => {
		expect(skipCounts(rows)).toEqual([['too_calm', 2], ['earnings', 1]]);
	});

	it('counts a stock skipped in several runs once', () => {
		const repeated = [d('2026-10-06T13:00:01', 'AAA', 'skipped', 'too_calm'), d('2026-10-06T10:00:02', 'AAA', 'skipped', 'too_calm')];
		expect(skipCounts(repeated)).toEqual([['too_calm', 1]]);
	});

	it('labels known reasons and humanizes unknown ones', () => {
		expect(decisionLabel('sector_full')).toBe('Sector full');
		expect(decisionLabel('some_new_reason')).toBe('some new reason');
		expect(decisionLabel('bought', 'short')).toBe('Shorted');
		expect(decisionLabel('preview', 'short')).toBe('Would short (preview)');
		expect(decisionLabel('too_calm', 'short')).toBe('Too calm');
	});
});
