<script lang="ts">
	import { isTauri, mockRiskReport } from '$lib/tauri/mock';
	import { getRiskReport } from '$lib/tauri/commands';
	import type { RiskReport } from '$lib/tauri/types';
	import RiskTiles from '$lib/components/risk/RiskTiles.svelte';
	import DrawdownChart from '$lib/components/risk/DrawdownChart.svelte';
	import IndustryBars from '$lib/components/risk/IndustryBars.svelte';
	import StressTest from '$lib/components/risk/StressTest.svelte';
	import PositionsTable from '$lib/components/risk/PositionsTable.svelte';

	let report = $state<RiskReport | null>(null);
	let error = $state<string | null>(null);
	let loading = $state(true);
	let asOf = $state<Date | null>(null);
	let started = false;
	let seq = 0;

	function load() {
		const mine = ++seq;
		loading = true;
		error = null;
		const p: Promise<RiskReport> = isTauri() ? getRiskReport() : Promise.resolve(mockRiskReport);
		p.then((r) => {
			if (mine !== seq) return;
			report = r;
			asOf = new Date();
		})
			.catch((e) => {
				if (mine !== seq) return;
				error = e instanceof Error ? e.message : String(e ?? 'Unknown error');
			})
			.finally(() => {
				if (mine === seq) loading = false;
			});
	}

	$effect(() => {
		if (started) return;
		started = true;
		load();
	});

	function handleKeydown(e: KeyboardEvent) {
		const t = e.target;
		if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t instanceof HTMLSelectElement) return;
		if (t instanceof HTMLElement && t.isContentEditable) return;
		if (e.metaKey || e.ctrlKey || e.altKey) return;
		if (e.key === 'r') load();
	}

	const asOfLabel = $derived(asOf ? asOf.toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' }) : '');
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="max-w-5xl mx-auto px-6 py-6">
	<div class="flex items-center justify-between mb-5">
		<div>
			<h1 class="text-xl font-bold text-text">Risk</h1>
			<p class="text-xs text-text-secondary mt-0.5">
				{#if report}
					{report.position_count} {report.position_count === 1 ? 'position' : 'positions'} · equity
					<span class="font-mono tabular-nums">${Math.round(report.equity).toLocaleString('en-US')}</span>
					{#if asOfLabel}· as of {asOfLabel}{/if}
				{:else}
					How much the account could lose, and where.
				{/if}
			</p>
		</div>
		<button type="button" onclick={load} disabled={loading}
			class="text-xs px-3 py-1.5 rounded-lg border border-border bg-bg-card text-text-secondary hover:text-text hover:bg-bg-card-hover disabled:opacity-50 focus-visible:outline-2 focus-visible:outline-ai">
			{loading ? 'Loading…' : 'Reload'} <kbd class="ml-1 font-mono text-[10px] text-text-muted">r</kbd>
		</button>
	</div>

	{#if error}
		<div class="bg-bg-card border border-border rounded-xl p-6 text-center" role="alert" data-testid="risk-error">
			<p class="text-sm text-rose-400 mb-1">Couldn't load the risk report</p>
			<p class="text-xs text-text-secondary mb-4 break-words">{error}</p>
			<button type="button" onclick={load}
				class="text-xs px-3 py-1.5 rounded-lg border border-border bg-bg text-text hover:bg-bg-card-hover focus-visible:outline-2 focus-visible:outline-ai">
				Try again
			</button>
		</div>
	{:else if !report}
		<div class="animate-pulse" aria-busy="true" aria-label="Loading risk report" data-testid="risk-skeleton">
			<div class="grid grid-cols-3 gap-3 mb-5">
				{#each Array(6) as _, i (i)}<div class="h-24 rounded-xl bg-bg-card border border-border"></div>{/each}
			</div>
			<div class="h-44 rounded-xl bg-bg-card border border-border mb-5"></div>
			<div class="h-64 rounded-xl bg-bg-card border border-border"></div>
		</div>
	{:else}
		{#if report.warnings.length > 0}
			<ul class="mb-5 space-y-1" data-testid="risk-warnings">
				{#each report.warnings as w (w)}
					<li class="text-xs text-amber-300/90 bg-amber-400/5 border border-amber-400/15 rounded-lg px-3 py-1.5">{w}</li>
				{/each}
			</ul>
		{/if}

		{#if report.position_count === 0 && report.positions.length === 0}
			<div class="bg-bg-card border border-border rounded-xl p-8 text-center mb-5" data-testid="risk-empty">
				<p class="text-sm text-text mb-1">No open positions</p>
				<p class="text-xs text-text-secondary">Everything is in cash ({report.cash_pct.toFixed(0)}%). Risk figures appear once the bot or you open a position.</p>
			</div>
			<DrawdownChart drawdown={report.drawdown} />
		{:else}
			<RiskTiles {report} />
			<DrawdownChart drawdown={report.drawdown} />
			<div class="grid grid-cols-1 xl:grid-cols-[3fr_2fr] gap-5 mb-5">
				<IndustryBars industries={report.industries} />
				<StressTest stress={report.stress} />
			</div>
			<PositionsTable positions={report.positions} />
		{/if}
	{/if}
</div>
