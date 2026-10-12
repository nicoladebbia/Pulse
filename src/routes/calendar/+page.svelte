<script lang="ts">
	import { isTauri, buildMockCalendar } from '$lib/tauri/mock';
	import { getCalendar } from '$lib/tauri/commands';
	import type { CalendarData, EarningsEvent, MacroEventView } from '$lib/tauri/types';
	import NextUp from '$lib/components/calendar/NextUp.svelte';
	import DayGroup from '$lib/components/calendar/DayGroup.svelte';

	const RANGES = [7, 14, 30, 60];
	type Filter = 'mine' | 'all' | 'macro';
	const FILTERS: { id: Filter; label: string }[] = [
		{ id: 'mine', label: 'Mine' },
		{ id: 'all', label: 'All followed' },
		{ id: 'macro', label: 'Macro only' }
	];

	let data = $state<CalendarData | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let days = $state(30);
	let filter = $state<Filter>('mine');
	let reqId = 0;

	async function load() {
		const id = ++reqId;
		loading = true;
		error = null;
		try {
			const res = isTauri()
				? await getCalendar(days).catch((e: unknown) => { throw new Error(String(e)); })
				: { ...buildMockCalendar(), days };
			if (id !== reqId) return;
			data = res;
		} catch (e) {
			if (id !== reqId) return;
			error = e instanceof Error ? e.message : String(e);
		} finally {
			if (id === reqId) loading = false;
		}
	}

	// onMount does not fire in this app (ssr=false + prerender); start from $effect.
	let started = false;
	$effect(() => {
		if (started) return;
		started = true;
		load();
	});

	function setDays(d: number) {
		if (d === days) return;
		days = d;
		load();
	}

	function onKey(e: KeyboardEvent) {
		if (e.key !== 'r' || e.metaKey || e.ctrlKey || e.altKey) return;
		const t = e.target as HTMLElement | null;
		if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable)) return;
		load();
	}

	const keep = (e: EarningsEvent) => e.held || (e.signal_score ?? 0) >= 0.2;
	const visibleEarnings = $derived(
		!data || filter === 'macro' ? [] : filter === 'mine' ? data.earnings.filter(keep) : data.earnings
	);
	const nextUp = $derived((data?.macro_events ?? []).filter((m) => m.days_until >= 0).slice(0, 3));

	const groups = $derived.by(() => {
		if (!data) return [];
		const map = new Map<string, { date: string; daysUntil: number; macro: MacroEventView[]; earnings: EarningsEvent[] }>();
		const slot = (date: string, d: number) => {
			let g = map.get(date);
			if (!g) { g = { date, daysUntil: d, macro: [], earnings: [] }; map.set(date, g); }
			return g;
		};
		for (const m of data.macro_events) slot(m.date, m.days_until).macro.push(m);
		for (const e of visibleEarnings) slot(e.date, e.days_until).earnings.push(e);
		const list = [...map.values()].sort((a, b) => a.date.localeCompare(b.date));
		for (const g of list) g.earnings.sort((a, b) => (b.held ? 1 : 0) - (a.held ? 1 : 0) || (b.signal_score ?? 0) - (a.signal_score ?? 0) || a.ticker.localeCompare(b.ticker));
		return list;
	});

	const emptyText = $derived(
		filter === 'mine'
			? "Nothing you hold or have a signal on reports in this window. Switch to All followed to see every company."
			: filter === 'all'
				? 'No earnings or macro dates in this window.'
				: 'No macro dates in this window.'
	);
</script>

<svelte:window onkeydown={onKey} />

<div class="max-w-4xl mx-auto px-6 py-8">
	<header class="flex items-center justify-between gap-4 mb-5 flex-wrap">
		<h1 class="text-2xl font-bold text-text">Calendar</h1>
		<div class="flex items-center gap-2">
			<div class="flex gap-1" role="group" aria-label="Range in days">
				{#each RANGES as r}
					<button
						class="text-xs font-mono tabular-nums px-2.5 py-1.5 rounded-lg border transition-colors focus-visible:outline-2 focus-visible:outline-ai {days === r ? 'bg-bg-card-hover border-text-muted text-text' : 'border-border text-text-secondary hover:text-text'}"
						aria-pressed={days === r}
						onclick={() => setDays(r)}
					>{r}d</button>
				{/each}
			</div>
			<button
				class="text-xs px-2.5 py-1.5 rounded-lg border border-border text-text-secondary hover:text-text disabled:opacity-50 focus-visible:outline-2 focus-visible:outline-ai"
				onclick={load}
				disabled={loading}
				aria-label="Reload calendar (r)"
				title="Reload (r)"
			>{loading ? 'Loading…' : 'Reload'}</button>
		</div>
	</header>

	{#if error}
		<div class="bg-bg-card border border-rose-400/30 rounded-xl p-6 text-center" role="alert">
			<p class="text-sm text-rose-400 mb-1">Couldn't load the calendar</p>
			<p class="text-xs text-text-muted mb-4 break-words">{error}</p>
			<button class="text-xs px-3 py-1.5 rounded-lg border border-border text-text hover:bg-bg-card-hover focus-visible:outline-2 focus-visible:outline-ai" onclick={load}>Try again</button>
		</div>
	{:else if !data}
		<div class="animate-pulse" aria-busy="true" aria-label="Loading calendar" data-testid="cal-skeleton">
			<div class="grid grid-cols-3 gap-3 mb-6">
				{#each [0, 1, 2] as i (i)}<div class="h-24 bg-bg-card border border-border rounded-xl"></div>{/each}
			</div>
			{#each [0, 1] as i (i)}
				<div class="h-4 w-40 bg-bg-card rounded mb-2"></div>
				<div class="h-28 bg-bg-card border border-border rounded-xl mb-5"></div>
			{/each}
		</div>
	{:else}
		{#if data.held_in_blackout.length > 0}
			<p class="mb-4 text-sm rounded-xl border border-amber-400/25 bg-amber-400/5 text-amber-300/90 px-4 py-3">
				You hold <span class="font-mono font-semibold">{data.held_in_blackout.join(', ')}</span>: reporting within 3 trading days, so Pulse won't add to them.
			</p>
		{/if}
		{#if !data.has_earnings_data}
			<p class="mb-4 text-xs rounded-xl border border-border bg-bg-card text-text-secondary px-4 py-3">
				Earnings dates aren't loaded yet. They come from Finnhub through the background fetch and will appear after its next run. Macro dates still show below.
			</p>
		{/if}

		<div class:opacity-60={loading} class="transition-opacity">
			<NextUp events={nextUp} />

			<div class="flex gap-2 mb-4 flex-wrap" role="group" aria-label="Filter">
				{#each FILTERS as f (f.id)}
					<button
						class="text-xs px-3 py-1.5 rounded-full border transition-colors focus-visible:outline-2 focus-visible:outline-ai {filter === f.id ? 'bg-bg-card-hover border-text-muted text-text' : 'border-border text-text-secondary hover:text-text'}"
						aria-pressed={filter === f.id}
						onclick={() => (filter = f.id)}
					>{f.label}</button>
				{/each}
			</div>

			{#if groups.length === 0}
				<p class="text-sm text-text-secondary bg-bg-card border border-border rounded-xl p-6 text-center" data-testid="cal-empty">{emptyText}</p>
			{:else}
				{#each groups as g (g.date)}
					<DayGroup date={g.date} daysUntil={g.daysUntil} macro={g.macro} earnings={g.earnings} />
				{/each}
			{/if}
		</div>
	{/if}
</div>
