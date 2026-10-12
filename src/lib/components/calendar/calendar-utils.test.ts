import { describe, expect, it } from 'vitest';
import { fmtDay, hourLabel, parseDay, relDays } from './calendar-utils';

describe('calendar utils', () => {
	it('parses a day as local time, not UTC', () => {
		const d = parseDay('2026-10-14');
		expect([d.getFullYear(), d.getMonth(), d.getDate()]).toEqual([2026, 9, 14]);
	});

	it('formats days with or without the weekday', () => {
		expect(fmtDay('2026-10-14')).toBe('Wed, Oct 14');
		expect(fmtDay('2026-10-14', false)).toBe('Oct 14');
	});

	it('says how far away a day is', () => {
		expect([0, 1, -1, 5, -3].map(relDays)).toEqual(['today', 'tomorrow', 'yesterday', 'in 5 days', '3 days ago']);
	});

	it('labels the report time and shows a dash when unknown', () => {
		expect([hourLabel('bmo'), hourLabel('amc'), hourLabel('dmh'), hourLabel(null), hourLabel('x')]).toEqual([
			'Before open', 'After close', 'During hours', '—', '—'
		]);
	});
});
