/**
 * Local calendar dates as `YYYY-MM-DD` strings.
 *
 * Every date the backend stores is a *local* date — the fetcher writes with
 * `chrono::Local::now()` and `get_today_freedoms` / `get_today_briefing` compare
 * against `Local` too. Any frontend that builds its date with `toISOString()` is
 * therefore asking a UTC question about local data, and in Miami (UTC-4/-5) the
 * two disagree for the four hours between 20:00 and midnight — prime reading
 * time. The Freedoms page had exactly that bug: after 20:00 it believed tomorrow
 * had started, so "← Previous day" re-rendered *today's* briefing under the
 * label "Yesterday".
 *
 * These helpers never touch UTC and never round-trip through `toISOString()`.
 */

function pad(n: number): string {
	return String(n).padStart(2, '0');
}

/** Today (or the given instant) as a local `YYYY-MM-DD`. */
export function localDateStr(d: Date = new Date()): string {
	return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/**
 * Move a `YYYY-MM-DD` string by whole days.
 *
 * Steps the calendar parts through the local-time `Date` constructor rather than
 * parsing and re-serialising through UTC, so month, year, leap-day and DST
 * boundaries all come out right — a naive `setDate(getDate() - 1)` on a
 * UTC-parsed date can land on the wrong day across a DST change.
 */
export function shiftDateStr(dateStr: string, days: number): string {
	const [y, m, d] = dateStr.split('-').map(Number);
	return localDateStr(new Date(y, m - 1, d + days));
}

/**
 * Chronological comparison of two `YYYY-MM-DD` strings. Zero-padded ISO dates
 * sort lexically in calendar order, so this needs no parsing at all — it is a
 * named function so call sites read as intent rather than as a string compare.
 */
export function isSameOrBefore(a: string, b: string): boolean {
	return a <= b;
}
