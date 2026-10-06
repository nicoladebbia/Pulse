<script lang="ts">
	// Weekly: which signal types and score ranges have beaten the S&P 500.
	import type { SignalScorecard, ScorecardRow } from '$lib/tauri/types';

	let { scorecard, error = null }: { scorecard: SignalScorecard | null; error?: string | null } = $props();

	const NAMES: Record<string, string> = {
		'all buy-grade': 'All buy-grade signals',
		insider: 'Insider buying',
		news: 'News momentum',
		government: 'Government (contracts, filings)',
		search: 'Search interest',
		institutional: 'Institutional (weight 0)',
		patent: 'Patents (weight 0)',
		supply_chain: 'Supply chain (weight 0)',
		political: 'Political (weight 0)',
	};
	/** Fewer samples than this and a group's numbers are noise. */
	const MIN_N = 10;

	const dims = $derived(
		(scorecard?.rows ?? [])
			.filter((r) => r.grp_kind === 'dimension')
			.sort((a, b) => (a.grp === 'all buy-grade' ? -1 : b.grp === 'all buy-grade' ? 1 : b.signals - a.signals))
	);
	const scores = $derived((scorecard?.rows ?? []).filter((r) => r.grp_kind === 'score'));

	const pts = (v: number | null) => (v == null ? '—' : `${v >= 0 ? '+' : ''}${v.toFixed(1)}`);
	const pct = (v: number | null) => (v == null ? '—' : `${Math.round(v * 100)}%`);
	const money = (v: number | null) =>
		v == null ? '—' : `${v >= 0 ? '+' : '−'}${Math.abs(v).toLocaleString('en-US', { style: 'currency', currency: 'USD', maximumFractionDigits: 0 })}`;
	const tone = (v: number | null, r: ScorecardRow) =>
		r.signals < MIN_N || v == null ? 'text-text-muted' : v >= 0 ? 'text-emerald-400' : 'text-rose-400';
</script>

{#snippet table(title: string, rows: ScorecardRow[], name: (g: string) => string)}
	<div class="mb-4">
		<div class="text-[11px] text-text-secondary font-medium mb-1.5">{title}</div>
		<table class="w-full text-[11px]">
			<thead>
				<tr class="text-text-muted text-left">
					<th class="font-normal pb-1"></th>
					<th class="font-normal pb-1 text-right" title="Signal episodes in the last 120 days">Signals</th>
					<th class="font-normal pb-1 text-right" title="Share that beat SPY over the next 10 trading days">Beat S&amp;P</th>
					<th class="font-normal pb-1 text-right" title="Average 10-day return minus SPY, in percentage points">Avg vs S&amp;P</th>
					<th class="font-normal pb-1 text-right" title="The bot's own closed trades in this group">Bot trades</th>
					<th class="font-normal pb-1 text-right">Bot P&amp;L</th>
				</tr>
			</thead>
			<tbody>
				{#each rows as r}
					<tr class="border-t border-border/40 {r.signals < MIN_N ? 'opacity-60' : ''}">
						<td class="py-1 text-text-secondary">{name(r.grp)}</td>
						<td class="py-1 text-right font-mono text-text-muted">{r.signals}</td>
						<td class="py-1 text-right font-mono {tone(r.win_rate == null ? null : r.win_rate - 0.5, r)}">{pct(r.win_rate)}</td>
						<td class="py-1 text-right font-mono {tone(r.avg_excess, r)}">{pts(r.avg_excess)}</td>
						<td class="py-1 text-right font-mono text-text-muted">{r.trades || '—'}{#if r.trades && r.trade_win_rate != null}<span class="text-text-muted"> ({pct(r.trade_win_rate)} won)</span>{/if}</td>
						<td class="py-1 text-right font-mono {r.trade_pnl == null ? 'text-text-muted' : r.trade_pnl >= 0 ? 'text-emerald-400' : 'text-rose-400'}">{money(r.trade_pnl)}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
{/snippet}

<div class="bg-bg-card border border-border rounded-xl p-4 mb-5" data-testid="signal-scorecard">
	<div class="flex items-center justify-between mb-3">
		<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider">Signal scorecard</h2>
		{#if scorecard?.computed_at}<span class="text-[10px] text-text-muted">updated {scorecard.computed_at} · weekly</span>{/if}
	</div>
	{#if error}
		<p class="text-xs text-rose-400">Couldn't load the scorecard: {error}</p>
	{:else if scorecard === null}
		<p class="text-xs text-text-muted">Loading…</p>
	{:else if scorecard.rows.length === 0}
		<p class="text-xs text-text-muted">No scorecard yet. It's built once a week by the scheduler (first run within an hour of this update).</p>
	{:else}
		{@render table('By signal type (signals the bot would buy)', dims, (g) => NAMES[g] ?? g)}
		{@render table('By score', scores, (g) => g)}
		<p class="text-[10px] text-text-muted">Every buy-grade signal of the last 120 days, bought or not, held from the next open for 10 trading days and compared with SPY. A stock signalling day after day counts once per 10 days. Faded rows have fewer than {MIN_N} signals — too few to trust.</p>
	{/if}
</div>
