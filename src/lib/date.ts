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

/**
 * "Today" / "Yesterday" / "3d ago" / "Aug 16" for a `YYYY-MM-DD`.
 *
 * Compares CALENDAR DAYS, not timestamps. The version this replaces anchored the
 * story at `T12:00:00` and subtracted a live `now`; before local noon that
 * difference is negative, so `Math.floor` returned -1 and every label was a day
 * out — today read "-1d ago" and yesterday read "Today". Observed on the Trends
 * page at 07:01 local, and the identical code was in ChatSources.
 *
 * `Math.round` rather than `floor`, because a DST transition makes one calendar
 * day 23 or 25 hours long and a floor over that lands on the wrong day. A future
 * date falls through to the formatted form rather than a negative count.
 */
export function relativeDayLabel(dateStr: string, now: Date = new Date()): string {
	const [y, m, d] = dateStr.split('-').map(Number);
	const then = new Date(y, m - 1, d);
	const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
	const diffDays = Math.round((today.getTime() - then.getTime()) / 86400000);

	if (diffDays === 0) return 'Today';
	if (diffDays === 1) return 'Yesterday';
	if (diffDays > 1 && diffDays < 7) return `${diffDays}d ago`;
	return then.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
}
