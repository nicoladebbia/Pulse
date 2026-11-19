// Labels for the prediction-market panel of the briefing page
// (`get_prediction_markets`). What is listed, and in what order, is decided by
// the fetcher; this only turns the numbers into words.
import type { PredictionMarket } from '$lib/tauri/types';

/** A chance of "Yes" (0..1) as a percentage. Near the ends it says so instead of rounding to 0% or 100%. */
export function oddsLabel(probability: number): string {
	if (!Number.isFinite(probability)) return '–';
	const pct = Math.min(1, Math.max(0, probability)) * 100;
	if (pct < 1) return '<1%';
	if (pct > 99) return '>99%';
	return `${Math.round(pct)}%`;
}

/** Whole points moved in 24 hours (0.05 is five points); 0 when there was no move worth a number. */
export function changePoints(change: number | null | undefined): number {
	if (change == null || !Number.isFinite(change)) return 0;
	const points = Math.round(change * 100);
	return points === 0 ? 0 : points;
}

/** "+12" / "−6" (a real minus sign), or an empty string for no move. */
export function changeLabel(change: number | null | undefined): string {
	const points = changePoints(change);
	if (points === 0) return '';
	return points > 0 ? `+${points}` : `−${Math.abs(points)}`;
}

/** Dollars traded, short: $1.2M, $504k, $900. */
export function volumeLabel(dollars: number): string {
	if (!Number.isFinite(dollars) || dollars <= 0) return '$0';
	if (dollars >= 999_500) {
		const millions = dollars / 1_000_000;
		return `$${millions >= 10 ? Math.round(millions) : Math.round(millions * 10) / 10}M`;
	}
	if (dollars >= 1_000) return `$${Math.round(dollars / 1_000)}k`;
	return `$${Math.round(dollars)}`;
}

const TOPICS: Record<string, string> = {
	ai: 'AI',
	tech: 'Tech',
	economy: 'Economy',
	finance: 'Finance',
	geopolitics: 'World',
	politics: 'Politics',
};

export function topicLabel(topic: string): string {
	return TOPICS[topic] ?? topic.replace(/_/g, ' ');
}

/**
 * When the odds were read, from the fetcher's local `YYYY-MM-DDTHH:MM:SS`:
 * "08:04" today, "yesterday 21:02", otherwise "Nov 17" (the app shows nothing
 * older than yesterday, so that is only a fallback). Compares calendar
 * days, as `relativeDayLabel` does. An unreadable stamp gives an empty string.
 */
export function asOfLabel(fetchedAt: string, now: Date = new Date()): string {
	const m = /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})/.exec(fetchedAt ?? '');
	if (!m) return '';
	const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
	const time = `${m[4]}:${m[5]}`;
	const day = new Date(y, mo - 1, d);
	const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
	const daysAgo = Math.round((today.getTime() - day.getTime()) / 86_400_000);
	if (daysAgo === 0) return time;
	if (daysAgo === 1) return `yesterday ${time}`;
	return day.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
}

/** The two lists of the panel, each in the order the fetcher stored. */
export function splitMarkets(markets: PredictionMarket[]): { movers: PredictionMarket[]; active: PredictionMarket[] } {
	return {
		movers: markets.filter((m) => m.kind === 'mover'),
		active: markets.filter((m) => m.kind !== 'mover'),
	};
}

/** Only an https link to the market's own site is opened; anything else is treated as no link. */
export function safeMarketUrl(url: string | null | undefined): string | null {
	if (!url) return null;
	return /^https:\/\/polymarket\.com\/event\/[a-z0-9-]+$/.test(url) ? url : null;
}
