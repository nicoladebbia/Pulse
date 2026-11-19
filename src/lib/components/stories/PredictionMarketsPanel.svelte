<script lang="ts">
	import type { PredictionMarket, PredictionMarkets } from '$lib/tauri/types';
	import { openExternal } from '$lib/tauri/shell';
	import { asOfLabel, changeLabel, changePoints, oddsLabel, safeMarketUrl, splitMarkets, topicLabel, volumeLabel } from '$lib/prediction-markets';

	let { data }: { data: PredictionMarkets } = $props();

	let lists = $derived(splitMarkets(data.markets));
	let asOf = $derived(asOfLabel(data.fetched_at));
	let sections = $derived(
		[
			{ title: 'Odds that moved in the last 24 hours', rows: lists.movers },
			{ title: 'Most money traded', rows: lists.active },
		].filter((s) => s.rows.length > 0)
	);
</script>

{#snippet cells(market: PredictionMarket)}
	{@const points = changePoints(market.change_1d)}
	<span class="text-[9px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded shrink-0 w-16 text-center text-text-secondary bg-border/60">
		{topicLabel(market.topic)}
	</span>
	<span class="text-sm text-text flex-1 line-clamp-2">{market.question}</span>
	<span class="text-xs text-text-muted shrink-0 hidden sm:inline tabular-nums" title="Dollars traded in the last 24 hours">
		{volumeLabel(market.volume_24h)}
	</span>
	<span
		class="text-xs shrink-0 w-10 text-right tabular-nums {points > 0 ? 'text-emerald-400' : points < 0 ? 'text-rose-400' : 'text-text-muted'}"
		title="Change in the last 24 hours, in percentage points"
	>
		{changeLabel(market.change_1d) || '·'}
	</span>
	<span class="text-sm font-semibold text-text shrink-0 w-12 text-right tabular-nums" title="Chance of yes, by what people are paying">
		{oddsLabel(market.probability)}
	</span>
{/snippet}

{#snippet row(market: PredictionMarket)}
	{@const url = safeMarketUrl(market.url)}
	{#if url}
		<button
			class="w-full flex items-center gap-3 py-2.5 px-3 rounded-lg text-left transition-colors hover:bg-bg-card-hover cursor-pointer"
			title="Open this market on Polymarket"
			onclick={() => openExternal(url)}
			data-market={market.market_id}
		>
			{@render cells(market)}
		</button>
	{:else}
		<div class="w-full flex items-center gap-3 py-2.5 px-3 rounded-lg text-left" data-market={market.market_id}>
			{@render cells(market)}
		</div>
	{/if}
{/snippet}

<div data-testid="prediction-markets">
	<div class="flex items-baseline justify-between mb-3">
		<h3 class="text-xs font-semibold uppercase tracking-wider text-text-muted">Prediction Markets</h3>
		<span class="text-[11px] text-text-muted">Polymarket{asOf ? ` · as of ${asOf}` : ''}</span>
	</div>
	<div class="bg-bg-card border border-border rounded-xl">
		{#each sections as section, i (section.title)}
			<div class="{i > 0 ? 'border-t border-border/50' : ''} p-1.5">
				<div class="flex items-center gap-3 px-3 pt-2 pb-1 text-[10px] uppercase tracking-wider text-text-muted">
					<span class="flex-1">{section.title}</span>
					<span class="shrink-0 hidden sm:inline">Traded</span>
					<span class="shrink-0 w-10 text-right">24h</span>
					<span class="shrink-0 w-12 text-right">Yes</span>
				</div>
				{#each section.rows as market (market.market_id)}
					{@render row(market)}
				{/each}
			</div>
		{/each}
		<p class="border-t border-border/50 px-4 py-2.5 text-[11px] leading-relaxed text-text-muted">
			The percentage is the chance of "yes" that people are betting real money on. It is here to read next to the
			news; Pulse does not trade on it.
		</p>
	</div>
</div>
