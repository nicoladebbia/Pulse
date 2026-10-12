import type { MacroKind } from '$lib/tauri/types';

/** Parse a YYYY-MM-DD string as a local date (no timezone shift). */
export function parseDay(s: string): Date {
	const [y, m, d] = s.split('-').map(Number);
	return new Date(y, (m || 1) - 1, d || 1);
}

export function fmtDay(s: string, withWeekday = true): string {
	return parseDay(s).toLocaleDateString('en-US', {
		weekday: withWeekday ? 'short' : undefined,
		month: 'short',
		day: 'numeric'
	});
}

export function relDays(n: number): string {
	if (n === 0) return 'today';
	if (n === 1) return 'tomorrow';
	if (n === -1) return 'yesterday';
	return n > 0 ? `in ${n} days` : `${-n} days ago`;
}

export const MACRO_LABEL: Record<MacroKind, string> = {
	fomc: 'FOMC',
	cpi: 'CPI',
	ppi: 'PPI',
	jobs: 'Jobs'
};

export function hourLabel(h: string | null): string {
	switch (h) {
		case 'bmo': return 'Before open';
		case 'amc': return 'After close';
		case 'dmh': return 'During hours';
		default: return '—';
	}
}
