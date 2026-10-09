// Grouping and labels for the "Why didn't it buy?" card (trade_decisions).
import type { TradeDecision } from '$lib/tauri/types';

const LABELS: Record<string, string> = {
	bought: 'Bought',
	preview: 'Would buy (preview)',
	earnings: 'Earnings soon',
	too_calm: 'Too calm',
	too_thin: 'Too thin or cheap',
	sector_full: 'Sector full',
	universe: 'Failed quality check',
	insider_selling: 'Insiders selling',
	new_listing: 'Too new',
	no_bars: 'No price history',
	no_cash: 'No cash',
	untracked_holding: 'Untracked holding',
	ticker_cap: 'Already at 8% cap',
	no_risk_room: 'Risk budget used',
	open_order: 'Order already open',
	price: 'Price check failed',
	order_failed: 'Order rejected',
	no_candidates: 'No strong signals',
	max_positions: 'Max positions',
	broker_unreadable: 'Alpaca unreadable',
	bars_refused: 'Price data refused',
};

const SHORT_LABELS: Record<string, string> = {
	bought: 'Shorted',
	preview: 'Would short (preview)',
};

export function decisionLabel(reason: string, direction = 'long'): string {
	if (direction === 'short' && SHORT_LABELS[reason]) return SHORT_LABELS[reason];
	return LABELS[reason] ?? reason.replace(/_/g, ' ');
}

/** Rows grouped by run, keeping the server's newest-first order. */
export function groupRuns(decisions: TradeDecision[]): { at: string; rows: TradeDecision[] }[] {
	const byRun = new Map<string, TradeDecision[]>();
	for (const d of decisions) {
		const list = byRun.get(d.run_at) ?? [];
		list.push(d);
		byRun.set(d.run_at, list);
	}
	return [...byRun.entries()].map(([at, rows]) => ({ at, rows }));
}

/** How often each skip reason came up, most common first. */
export function skipCounts(decisions: TradeDecision[]): [string, number][] {
	// Distinct stocks per reason: a name skipped in every run counts once.
	const tickers = new Map<string, Set<string>>();
	for (const d of decisions) {
		if (d.outcome !== 'skipped') continue;
		const set = tickers.get(d.reason) ?? new Set<string>();
		set.add(d.ticker || d.name || '');
		tickers.set(d.reason, set);
	}
	return [...tickers.entries()].map(([r, set]): [string, number] => [r, set.size]).sort((a, b) => b[1] - a[1]);
}
