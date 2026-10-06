<script lang="ts">
	// "Why didn't it buy?" — what each recent auto-trade run did with each
	// candidate, newest run first.
	import type { TradeDecision } from '$lib/tauri/types';
	import { decisionLabel, groupRuns, skipCounts } from '$lib/trade-decisions';

	let { decisions, error = null }: { decisions: TradeDecision[] | null; error?: string | null } = $props();

	const label = decisionLabel;
	const runs = $derived(groupRuns(decisions ?? []));
	const reasonCounts = $derived(skipCounts(decisions ?? []));

	let showOlder = $state(false);

	function when(at: string): string {
		const d = new Date(at);
		if (Number.isNaN(d.getTime())) return at;
		return d.toLocaleString('en-US', { weekday: 'short', hour: 'numeric', minute: '2-digit' });
	}
	function chip(outcome: string): string {
		if (outcome === 'bought') return 'bg-emerald-500/15 text-emerald-400';
		if (outcome === 'preview') return 'bg-blue-500/15 text-blue-400';
		if (outcome === 'run_stopped') return 'bg-amber-500/15 text-amber-400';
		return 'bg-zinc-500/15 text-zinc-400';
	}
</script>

<div class="bg-bg-card border border-border rounded-xl p-4 mb-5" data-testid="trade-decisions">
	<div class="flex items-center justify-between mb-2">
		<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider">Why didn't it buy?</h2>
		<span class="text-[10px] text-text-muted">last 3 days · buys run from 10:00 and 13:00 New York time</span>
	</div>

	{#if error}
		<p class="text-xs text-rose-400">Couldn't load the auto-trade log: {error}</p>
	{:else if decisions === null}
		<p class="text-xs text-text-muted">Loading…</p>
	{:else if runs.length === 0}
		<p class="text-xs text-text-muted">No auto-trade runs logged yet. The log starts with the first market-hours run after this update.</p>
	{:else}
		{#if reasonCounts.length > 0}
			<div class="flex flex-wrap gap-1.5 mb-3">
				{#each reasonCounts as [reason, n]}
					<span class="text-[10px] px-1.5 py-0.5 rounded bg-zinc-500/10 text-text-secondary">{label(reason)} · {n}</span>
				{/each}
			</div>
		{/if}
		{#each runs as run, i}
			{#if i === 0 || showOlder}
				<div class="{i > 0 ? 'mt-3 pt-3 border-t border-border/50' : ''}">
					<div class="text-[11px] text-text-muted mb-1.5">{i === 0 ? 'Latest run' : 'Run'} · {when(run.at)}</div>
					<div class="space-y-1">
						{#each run.rows as d}
							<div class="flex items-start gap-2 text-[11px]">
								<span class="shrink-0 px-1.5 py-0.5 rounded font-medium {chip(d.outcome)}">{label(d.reason)}</span>
								{#if d.ticker}
									<span class="font-mono font-semibold text-text shrink-0">{d.ticker}</span>
									{#if d.score != null}<span class="font-mono text-text-muted shrink-0">{d.score.toFixed(2)}</span>{/if}
								{/if}
								<span class="text-text-secondary">{d.detail ?? ''}</span>
							</div>
						{/each}
					</div>
				</div>
			{/if}
		{/each}
		{#if runs.length > 1}
			<button onclick={() => (showOlder = !showOlder)} class="mt-3 text-[11px] text-blue-400 hover:text-blue-300 transition-colors">
				{showOlder ? 'Hide earlier runs' : `Show ${runs.length - 1} earlier run${runs.length > 2 ? 's' : ''}`}
			</button>
		{/if}
	{/if}
</div>
