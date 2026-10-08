<script lang="ts">
	// Daily: what the bot learned from its closed trades and from the signals it
	// bought or skipped (pulse-fetcher --mode learn).
	import type { LearningReport } from '$lib/tauri/types';

	let { learning, error = null }: { learning: LearningReport | null; error?: string | null } = $props();

	const SOURCES: Record<string, string> = {
		insider: 'Insider buying',
		news: 'News momentum',
		government: 'Government',
		search: 'Search interest',
		institutional: 'Institutional',
		patent: 'Patents',
		supply_chain: 'Supply chain',
		political: 'Political',
		insider_signal: 'Insider buying',
		news_momentum: 'News momentum',
		government_signal: 'Government',
		search_trend: 'Search interest',
		institutional_flow: 'Institutional',
		patent_signal: 'Patents',
		political_signal: 'Political',
	};
	const EXITS: Record<string, string> = {
		trailing_stop: 'Trailing stop',
		hard_stop: 'Hard stop (−15%)',
		fixed_stop: 'Fixed stop',
		broker_stop: 'Broker stop',
		signal_decay: 'Signal faded',
		profit_target: 'Profit target',
		closed_between_runs: 'Sold between runs',
		reconcile: 'Reconciled',
		other: 'Other',
	};
	const LESSONS: Record<string, string> = {
		beat_market: 'beat the market',
		lagged_market: 'trailed the market',
		gave_back_gains: 'gave back gains',
		stopped_then_recovered: 'stopped, then recovered',
		sold_early: 'sold too early',
		good_exit: 'good exit',
		deep_drawdown: 'deep drawdown',
		quick_loss: 'quick loss',
	};
	const GATES: Record<string, string> = {
		bought: 'Bought',
		too_calm: 'Skipped: moves too little',
		too_thin: 'Skipped: too thinly traded',
		earnings: 'Skipped: earnings soon',
		universe: 'Skipped: too small / not tradable',
		sector_full: 'Skipped: sector full',
		insider_selling: 'Skipped: insiders selling',
		ticker_cap: 'Skipped: per-stock cap',
		no_risk_room: 'Skipped: no risk budget',
		untracked_holding: 'Skipped: untracked holding',
		open_order: 'Skipped: order already open',
		no_cash: 'Skipped: no cash',
		not_shortable: "Skipped: can't be shorted",
	};

	const EVENTS: Record<string, string> = {
		news_surprise: 'News tone jump',
		insider_cluster: 'Insider cluster buy',
	};

	const body = $derived(learning?.report ?? null);
	const pts = (v: number | null | undefined) => (v == null ? '—' : `${v >= 0 ? '+' : ''}${v.toFixed(1)}`);
	const tone = (v: number | null | undefined) =>
		v == null ? 'text-text-muted' : v >= 0 ? 'text-emerald-400' : 'text-rose-400';
	/** |t| below 2 is not distinguishable from luck. */
	const sure = (t: number, n: number) => n >= 10 && Math.abs(t) >= 2;
</script>

<div class="bg-bg-card border border-border rounded-xl p-4 mb-5" data-testid="learning-report">
	<div class="flex items-center justify-between mb-3">
		<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider">What Pulse learned</h2>
		{#if learning?.computed_at}<span class="text-[10px] text-text-muted">updated {learning.computed_at} · daily</span>{/if}
	</div>
	{#if error}
		<p class="text-xs text-rose-400">Couldn't load the learning report: {error}</p>
	{:else if learning === null}
		<p class="text-xs text-text-muted">Loading…</p>
	{:else if !body}
		<p class="text-xs text-text-muted">No report yet. It's built once a day after the US market closes.</p>
	{:else}
		<ul class="mb-4 space-y-1">
			{#each body.headline as line}
				<li class="text-xs text-text-secondary">{line}</li>
			{/each}
		</ul>

		{#if body.weight_changes.length > 0}
			<div class="mb-4 rounded-lg border border-border/60 p-2.5">
				<div class="text-[11px] font-medium text-text-secondary mb-1">
					{learning.weights_applied ? 'Signal weights changed' : 'Suggested weight changes (not applied)'}
				</div>
				{#each body.weight_changes as c}
					<div class="text-[11px] text-text-muted">
						<span class="text-text-secondary">{SOURCES[c.dimension] ?? c.dimension}</span>
						{(c.from * 100).toFixed(1)}% → <span class={tone(c.to - c.from)}>{(c.to * 100).toFixed(1)}%</span>
						· {c.why}
					</div>
				{/each}
			</div>
		{/if}

		<div class="grid gap-4 md:grid-cols-2 mb-4">
			<div>
				<div class="text-[11px] text-text-secondary font-medium mb-1.5">Each source, 10 days after its signals</div>
				<table class="w-full text-[11px]">
					<thead>
						<tr class="text-text-muted text-left">
							<th class="font-normal pb-1"></th>
							<th class="font-normal pb-1 text-right">Signals</th>
							<th class="font-normal pb-1 text-right" title="Average 10-day return minus SPY, shrunk toward zero for small samples">vs S&amp;P</th>
							<th class="font-normal pb-1 text-right" title="The bot's own trades on this source, vs SPY">Bot trades</th>
						</tr>
					</thead>
					<tbody>
						{#each body.sources as s}
							<tr class="border-t border-border/40 {sure(s.t, s.signals) ? '' : 'opacity-60'}">
								<td class="py-1 text-text-secondary">{SOURCES[s.dimension] ?? s.dimension}</td>
								<td class="py-1 text-right font-mono text-text-muted">{s.signals}</td>
								<td class="py-1 text-right font-mono {tone(s.shrunk_excess)}">{pts(s.shrunk_excess)}</td>
								<td class="py-1 text-right font-mono {tone(s.trade_avg_excess)}">{s.trades ? `${s.trades} · ${pts(s.trade_avg_excess)}` : '—'}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
			<div>
				<div class="text-[11px] text-text-secondary font-medium mb-1.5">Each way out</div>
				<table class="w-full text-[11px]">
					<thead>
						<tr class="text-text-muted text-left">
							<th class="font-normal pb-1"></th>
							<th class="font-normal pb-1 text-right">Trades</th>
							<th class="font-normal pb-1 text-right">Avg return</th>
							<th class="font-normal pb-1 text-right" title="Average move in the 10 trading days after the sale: positive means it sold too early">10d after</th>
						</tr>
					</thead>
					<tbody>
						{#each body.exits as e}
							<tr class="border-t border-border/40">
								<td class="py-1 text-text-secondary">{EXITS[e.exit_kind] ?? e.exit_kind}</td>
								<td class="py-1 text-right font-mono text-text-muted">{e.trades}</td>
								<td class="py-1 text-right font-mono {tone(e.avg_return)}">{pts(e.avg_return)}%</td>
								<td class="py-1 text-right font-mono text-text-muted">{pts(e.avg_after10)}{e.avg_after10 == null ? '' : '%'}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		</div>

		{#if body.events && body.events.length > 0}
			<div class="mb-4">
				<div class="text-[11px] text-text-secondary font-medium mb-1.5">Event signals, over their holding period</div>
				<table class="w-full text-[11px]">
					<thead>
						<tr class="text-text-muted text-left">
							<th class="font-normal pb-1"></th>
							<th class="font-normal pb-1 text-right">Signals</th>
							<th class="font-normal pb-1 text-right" title="Average result vs SPY if every one had been traded (shorts counted as shorts)">If all traded</th>
							<th class="font-normal pb-1 text-right">Bot trades</th>
						</tr>
					</thead>
					<tbody>
						{#each body.events as e}
							<tr class="border-t border-border/40 {sure(e.t, e.signals) ? '' : 'opacity-60'}">
								<td class="py-1 text-text-secondary">{EVENTS[e.kind] ?? e.kind} · {e.direction} · {e.hold_days}d</td>
								<td class="py-1 text-right font-mono text-text-muted">{e.signals}</td>
								<td class="py-1 text-right font-mono {tone(e.avg_excess)}">{pts(e.avg_excess)}</td>
								<td class="py-1 text-right font-mono {tone(e.trade_avg_excess)}">{e.trades ? `${e.trades} · ${pts(e.trade_avg_excess)}` : '—'}</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}

		{#if body.gates && body.gates.length > 0}
			<div class="mb-4">
				<div class="text-[11px] text-text-secondary font-medium mb-1.5">Bought vs skipped, 10 days later</div>
				<table class="w-full text-[11px]">
					<tbody>
						{#each body.gates as g}
							<tr class="border-t border-border/40 {sure(g.t, g.signals) ? '' : 'opacity-60'}">
								<td class="py-1 text-text-secondary">{GATES[g.reason] ?? (g.outcome === 'bought' ? 'Bought' : `Skipped: ${g.reason}`)}</td>
								<td class="py-1 text-right font-mono text-text-muted">{g.signals}</td>
								<td class="py-1 text-right font-mono {tone(g.avg_excess)}">{pts(g.avg_excess)} vs S&amp;P</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		{/if}

		{#if learning.reviews.length > 0}
			<div class="text-[11px] text-text-secondary font-medium mb-1.5">Latest trades, start to finish</div>
			<div class="space-y-2 mb-3">
				{#each learning.reviews as r}
					<div class="border-t border-border/40 pt-2">
						<div class="flex items-center gap-2 text-[11px]">
							<span class="font-mono text-text">{r.ticker}</span>
							<span class="font-mono {tone(r.return_pct)}">{pts(r.return_pct)}%</span>
							{#if r.excess_pct != null}<span class="text-text-muted">({pts(r.excess_pct)} vs S&amp;P)</span>{/if}
							<span class="text-text-muted">· {EXITS[r.exit_kind] ?? r.exit_kind}</span>
							{#each r.lessons.filter((l) => l !== 'beat_market' && l !== 'lagged_market') as l}
								<span class="rounded bg-bg-expanded px-1.5 py-0.5 text-[10px] text-text-muted">{LESSONS[l] ?? l}</span>
							{/each}
						</div>
						<p class="text-[11px] text-text-muted mt-0.5">{r.story}</p>
					</div>
				{/each}
			</div>
		{/if}

		<p class="text-[10px] text-text-muted">
			Every buy-grade signal of the last 120 days, bought or not, measured over the next 10 trading days against SPY.
			Small samples are pulled toward zero; faded rows aren't distinguishable from luck yet. Weights move at most 10% a
			week, only on clear evidence, and stay within half to one-and-a-half times their default.
		</p>
	{/if}
</div>
