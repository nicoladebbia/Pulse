<script lang="ts">
	import type { RiskReport } from '$lib/tauri/types';
	import { usd, pct, tone } from './format';

	let { report }: { report: RiskReport } = $props();
	const dd = $derived(report.drawdown);
</script>

<div class="grid grid-cols-3 gap-3 mb-5" data-testid="risk-tiles">
	<div class="bg-bg-card border border-border rounded-xl p-4">
		<div class="text-[10px] text-text-secondary uppercase tracking-wider">Gross exposure</div>
		<div class="text-2xl font-mono font-bold text-text tabular-nums">{pct(report.gross_pct)}</div>
		<div class="text-[11px] text-text-secondary">
			Long {usd(report.long_value)} · short {usd(report.short_value)}
		</div>
	</div>
	<div class="bg-bg-card border border-border rounded-xl p-4">
		<div class="text-[10px] text-text-secondary uppercase tracking-wider">Net exposure</div>
		<div class="text-2xl font-mono font-bold text-text tabular-nums">{pct(report.net_pct)}</div>
		<div class="text-[11px] text-text-secondary">Long minus short, of {usd(report.equity)}</div>
	</div>
	<div class="bg-bg-card border border-border rounded-xl p-4">
		<div class="text-[10px] text-text-secondary uppercase tracking-wider">Cash</div>
		<div class="text-2xl font-mono font-bold text-text tabular-nums">{pct(report.cash_pct)}</div>
		<div class="text-[11px] text-text-secondary">{usd(report.cash)} uninvested</div>
	</div>

	<div class="bg-bg-card border border-border rounded-xl p-4">
		<div class="text-[10px] text-text-secondary uppercase tracking-wider">If every stop hits</div>
		<div class="text-2xl font-mono font-bold text-rose-400 tabular-nums">
			{report.stop_loss_dollars > 0 ? '−' : ''}{usd(report.stop_loss_dollars)}
		</div>
		<div class="text-[11px] text-text-secondary tabular-nums">{pct(report.stop_loss_pct)} of the account</div>
		{#if report.positions_without_stop > 0}
			<div class="text-[11px] text-amber-400 mt-1">
				{report.positions_without_stop}
				{report.positions_without_stop === 1 ? 'position has' : 'positions have'} no stop, so they aren't counted
			</div>
		{/if}
	</div>
	<div class="bg-bg-card border border-border rounded-xl p-4">
		<div class="text-[10px] text-text-secondary uppercase tracking-wider">Market sensitivity</div>
		{#if report.beta_exposure_pct == null}
			<div class="text-2xl font-mono font-bold text-text-muted">—</div>
			<div class="text-[11px] text-text-secondary">No betas available yet, so this can't be measured.</div>
		{:else}
			<div class="text-2xl font-mono font-bold text-text tabular-nums">{pct(report.beta_exposure_pct)}</div>
			<div class="text-[11px] text-text-secondary">
				Beta-weighted: moves like this much S&amp;P 500 exposure. Measured for {pct(report.beta_coverage_pct, 0)} of the book.
			</div>
		{/if}
	</div>
	<div class="bg-bg-card border border-border rounded-xl p-4">
		<div class="text-[10px] text-text-secondary uppercase tracking-wider">Drawdown</div>
		<div class="text-2xl font-mono font-bold tabular-nums {tone(dd.current_pct)}">{pct(dd.current_pct)}</div>
		<div class="text-[11px] text-text-secondary tabular-nums">
			{dd.current_pct === 0 ? 'At a new high. ' : 'Below the peak. '}Worst {pct(dd.max_pct)}
		</div>
	</div>
</div>
