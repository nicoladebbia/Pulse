<script lang="ts">
	import type { EarningsEvent, MacroEventView } from '$lib/tauri/types';
	import MacroChip from './MacroChip.svelte';
	import { fmtDay, relDays, hourLabel } from './calendar-utils';

	let { date, daysUntil, macro, earnings }: {
		date: string;
		daysUntil: number;
		macro: MacroEventView[];
		earnings: EarningsEvent[];
	} = $props();

	const CAP = 40;
	let expanded = $state(false);
	const shown = $derived(expanded ? earnings : earnings.slice(0, CAP));
	const hidden = $derived(earnings.length - shown.length);
	const isToday = $derived(daysUntil === 0);
	const isPast = $derived(daysUntil < 0);
</script>

<section class="mb-5 {isPast ? 'opacity-60' : ''}" aria-labelledby="day-{date}">
	<h3 id="day-{date}" class="flex items-baseline gap-2 mb-2 text-sm">
		{#if isToday}
			<span class="font-semibold text-amber-400">Today</span>
			<span class="text-text-secondary">{fmtDay(date)}</span>
		{:else}
			<span class="font-semibold text-text">{fmtDay(date)}</span>
			<span class="text-text-muted">{relDays(daysUntil)}</span>
		{/if}
		{#if isPast}<span class="text-[11px] text-text-muted">reported</span>{/if}
	</h3>

	<ul class="list-none p-0 m-0 bg-bg-card border rounded-xl overflow-hidden divide-y divide-border-subtle {isToday ? 'border-amber-400/40' : 'border-border'}">
		{#each macro as ev (ev.kind)}
			<li class="flex items-center gap-3 px-4 py-2.5 bg-ai-dim/30 border-l-2 border-ai">
				<MacroChip kind={ev.kind} />
				<span class="text-sm font-medium text-text min-w-0 truncate">{ev.title}</span>
				<span class="ml-auto text-xs text-text-secondary tabular-nums whitespace-nowrap">{ev.time_et} ET</span>
			</li>
		{/each}
		{#each shown as e (e.ticker + e.date)}
			<li class="flex items-center gap-3 px-4 py-2 min-w-0">
				<span class="font-mono font-semibold text-sm text-text w-16 shrink-0">{e.ticker}</span>
				<div class="min-w-0 flex-1">
					<div class="text-sm text-text-secondary truncate">{e.name ?? ''}</div>
					{#if e.held || e.blackout || (e.signal_score ?? 0) >= 0.2}
					<span class="flex flex-wrap items-center gap-1.5 mt-1">
					{#if e.held}
						<span class="text-[10px] font-bold rounded px-1.5 py-0.5 bg-emerald-400/15 text-emerald-400 border border-emerald-400/30">HELD</span>
					{/if}
					{#if e.signal_score != null && e.signal_score >= 0.2}
						<span class="text-[10px] font-semibold rounded px-1.5 py-0.5 bg-bg text-text-secondary border border-border">SIGNAL <span class="font-mono tabular-nums">{e.signal_score.toFixed(2)}</span></span>
					{/if}
					{#if e.blackout}
						<span class="text-[10px] rounded px-1.5 py-0.5 bg-amber-400/10 text-amber-400/80 border border-amber-400/20">No new buys</span>
					{/if}
				</span>
					{/if}
				</div>
				<span class="text-xs text-text-muted w-20 shrink-0 text-right">{hourLabel(e.hour)}</span>
				<span class="text-xs font-mono tabular-nums text-text-secondary w-20 shrink-0 text-right">{e.eps_estimate != null ? `EPS ${e.eps_estimate.toFixed(2)}` : ''}</span>
			</li>
		{/each}
	</ul>
	{#if hidden > 0}
		<button class="mt-2 text-xs text-text-secondary hover:text-text focus-visible:outline-2 focus-visible:outline-ai rounded px-1" onclick={() => (expanded = true)}>Show {hidden} more</button>
	{/if}
</section>
