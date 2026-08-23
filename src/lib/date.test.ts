import { describe, it, expect } from 'vitest';
import { localDateStr, shiftDateStr, isSameOrBefore, relativeDayLabel } from './date';

/**
 * These guard a defect that only appears in the evening. The Freedoms page built
 * its "today" with `toISOString()`, which is always UTC. In Miami (UTC-4/-5) the
 * UTC calendar date rolls over at 20:00 local, so between 20:00 and midnight the
 * page believed tomorrow had already started — and clicking "Previous day"
 * rendered *today's* briefing labelled "Yesterday".
 *
 * The tests below pin the local-time behaviour by constructing dates with the
 * local-time Date constructor, which is what the app actually sees.
 */

describe('localDateStr', () => {
	it('uses local calendar parts, not the UTC ones', () => {
		// 2026-08-14 22:24 local. In any timezone behind UTC by 2h or more, the UTC
		// date here is already 2026-08-15 — toISOString() would return the wrong day.
		const evening = new Date(2026, 7, 14, 22, 24, 0);
		expect(localDateStr(evening)).toBe('2026-08-14');
	});

	it('pads single-digit months and days', () => {
		expect(localDateStr(new Date(2026, 0, 5, 9, 0, 0))).toBe('2026-01-05');
	});

	it('does not drift at either end of the day', () => {
		expect(localDateStr(new Date(2026, 5, 30, 0, 0, 0))).toBe('2026-06-30');
		expect(localDateStr(new Date(2026, 5, 30, 23, 59, 59))).toBe('2026-06-30');
	});

	it('agrees with the runtime local date when called with no argument', () => {
		const now = new Date();
		const expected = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
		expect(localDateStr()).toBe(expected);
	});
});

describe('shiftDateStr', () => {
	it('steps back a day', () => {
		expect(shiftDateStr('2026-08-15', -1)).toBe('2026-08-14');
	});

	it('steps forward a day', () => {
		expect(shiftDateStr('2026-08-14', 1)).toBe('2026-08-15');
	});

	it('crosses a month boundary', () => {
		expect(shiftDateStr('2026-09-01', -1)).toBe('2026-08-31');
		expect(shiftDateStr('2026-08-31', 1)).toBe('2026-09-01');
	});

	it('crosses a year boundary', () => {
		expect(shiftDateStr('2027-01-01', -1)).toBe('2026-12-31');
	});

	it('handles a leap day', () => {
		expect(shiftDateStr('2028-02-28', 1)).toBe('2028-02-29');
		expect(shiftDateStr('2028-03-01', -1)).toBe('2028-02-29');
	});

	it('crosses a DST boundary without losing or gaining a day', () => {
		// US DST starts 2026-03-08 and ends 2026-11-01. A naive
		// `setDate(getDate()-1)` on a UTC-parsed date can land on the wrong day
		// here; stepping the calendar parts cannot.
		expect(shiftDateStr('2026-03-09', -1)).toBe('2026-03-08');
		expect(shiftDateStr('2026-03-08', -1)).toBe('2026-03-07');
		expect(shiftDateStr('2026-11-02', -1)).toBe('2026-11-01');
		expect(shiftDateStr('2026-11-01', -1)).toBe('2026-10-31');
	});

	it('is its own inverse', () => {
		expect(shiftDateStr(shiftDateStr('2026-08-15', -7), 7)).toBe('2026-08-15');
	});
});

describe('isSameOrBefore', () => {
	it('compares plain YYYY-MM-DD strings lexically, which is chronological', () => {
		expect(isSameOrBefore('2026-08-14', '2026-08-15')).toBe(true);
		expect(isSameOrBefore('2026-08-15', '2026-08-15')).toBe(true);
		expect(isSameOrBefore('2026-08-16', '2026-08-15')).toBe(false);
	});

	it('orders across month and year boundaries', () => {
		expect(isSameOrBefore('2026-09-01', '2026-10-01')).toBe(true);
		expect(isSameOrBefore('2027-01-01', '2026-12-31')).toBe(false);
	});
});

describe('relativeDayLabel', () => {
	// The paid bug: Trends anchored each story at `T12:00:00` and subtracted a live
	// `now`. Before local noon that difference is NEGATIVE, so `Math.floor` gave -1
	// and every label shifted by a day — today rendered as "-1d ago" and yesterday
	// as "Today". Observed live at 07:01 on 2026-08-23.
	it('says Today for today even in the morning, when the old code said -1d ago', () => {
		const morning = new Date(2026, 7, 23, 7, 1, 0);
		expect(relativeDayLabel('2026-08-23', morning)).toBe('Today');
		expect(relativeDayLabel('2026-08-22', morning)).toBe('Yesterday');
	});

	it('gives the same answers late at night as it does in the morning', () => {
		const morning = new Date(2026, 7, 23, 0, 5, 0);
		const night = new Date(2026, 7, 23, 23, 55, 0);
		for (const day of ['2026-08-23', '2026-08-22', '2026-08-20', '2026-07-30']) {
			expect(relativeDayLabel(day, morning)).toBe(relativeDayLabel(day, night));
		}
	});

	it('counts whole days up to a week, then shows the date', () => {
		const now = new Date(2026, 7, 23, 9, 0, 0);
		expect(relativeDayLabel('2026-08-20', now)).toBe('3d ago');
		expect(relativeDayLabel('2026-08-17', now)).toBe('6d ago');
		// 7 days is where the relative form stops being easier to read than the date.
		expect(relativeDayLabel('2026-08-16', now)).toBe('Aug 16');
	});

	it('shows a future date as a date, never as a negative day count', () => {
		const now = new Date(2026, 7, 23, 9, 0, 0);
		expect(relativeDayLabel('2026-08-25', now)).toBe('Aug 25');
	});

	it('survives a DST boundary, where a calendar day is 23 or 25 hours long', () => {
		// US DST ends 2026-11-01, so 2026-11-01 local is 25 hours long.
		const afterFallBack = new Date(2026, 10, 2, 9, 0, 0);
		expect(relativeDayLabel('2026-11-01', afterFallBack)).toBe('Yesterday');
		expect(relativeDayLabel('2026-11-02', afterFallBack)).toBe('Today');
	});
});
