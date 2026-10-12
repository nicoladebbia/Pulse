export function usd(v: number | null | undefined, signed = false): string {
	if (v == null || !Number.isFinite(v)) return '—';
	const s = Math.abs(v).toLocaleString('en-US', { style: 'currency', currency: 'USD', maximumFractionDigits: 0 });
	if (!signed) return v < 0 ? `−${s}` : s;
	return `${v >= 0 ? '+' : '−'}${s}`;
}
export function pct(v: number | null | undefined, digits = 1, signed = false): string {
	if (v == null || !Number.isFinite(v)) return '—';
	const sign = v < 0 ? '−' : signed && v > 0 ? '+' : '';
	return `${sign}${Math.abs(v).toFixed(digits)}%`;
}
export const tone = (v: number) => (v > 0 ? 'text-emerald-400' : v < 0 ? 'text-rose-400' : 'text-text-secondary');
/** The per-industry cap the auto-trader enforces (entry_filters.rs), in % of equity. */
export const INDUSTRY_CAP_PCT = 30;
