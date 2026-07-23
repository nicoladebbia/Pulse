// signal_profile is a JSON string with three historical shapes:
// auto trades  → {"government":0.7,...,"stories":[...]}  (flat signal scores)
// manual       → {"source":"manual","confidence":...}
// reconciled   → {"source":"reconciled"}
// Older stories entries may be plain strings; newer ones {headline, source}.
export type TradeReason =
	| { kind: 'auto'; signals: { label: string; score: number }[]; stories: { title: string; source: string | null }[] }
	| { kind: 'manual' }
	| { kind: 'reconciled' };

// pipeline.rs::load_calibrated_weights hardcodes institutional_flow and
// supply_chain to ZERO weight in the compound score that actually gates
// entries — institutional_flow fires on a known substring-match bug (ticker
// "X" matched 61 funds), supply_chain is a market-wide constant with no
// per-entity signal. Their raw values are still stored in signal_profile for
// audit purposes, but they carry zero decision weight, so they must NEVER be
// shown as part of "why this trade fired" — that's misleading by omission of
// their true (non-)role. Excluded here at the source so every consumer
// (position card convergence line, fallback signal bars) inherits the fix.
const ZERO_WEIGHT_SIGNAL_KEYS = new Set(['institutional', 'supply_chain']);

export function parseTradeReason(profileJson: string | null | undefined): TradeReason | null {
	if (!profileJson) return null;
	try {
		const p = JSON.parse(profileJson);
		if (typeof p !== 'object' || p === null) return null;
		if (p.source === 'manual') return { kind: 'manual' };
		if (p.source === 'reconciled') return { kind: 'reconciled' };
		const signals = Object.entries(p)
			.filter(([k, v]) => k !== 'stories' && !ZERO_WEIGHT_SIGNAL_KEYS.has(k) && typeof v === 'number' && v > 0.05)
			.map(([k, v]) => ({ label: k.replace(/_/g, ' '), score: v as number }))
			.sort((a, b) => b.score - a.score);
		const stories = (Array.isArray(p.stories) ? p.stories : [])
			.map((s: any) => {
				if (typeof s === 'string') return { title: s, source: null };
				const t = s?.headline ?? s?.title;
				return typeof t === 'string' ? { title: t, source: s.source ?? null } : null;
			})
			.filter((s: any): s is { title: string; source: string | null } => s !== null)
			.slice(0, 3);
		if (signals.length === 0 && stories.length === 0) return null;
		return { kind: 'auto', signals, stories };
	} catch {
		return null;
	}
}

export function fmtReasonSignals(signals: { label: string; score: number }[]): string {
	return signals.slice(0, 4).map(s => `${s.label} ${(s.score * 100).toFixed(0)}%`).join(' · ');
}
