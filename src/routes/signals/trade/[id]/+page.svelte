<script lang="ts">
	import { page } from '$app/stores';
	import { isTauri } from '$lib/tauri/mock';
	import { getTradeDetail } from '$lib/tauri/commands';
	import type { TradeDetail, TradeSignalSnapshot } from '$lib/tauri/types';
	import { parseTradeReason } from '$lib/trade-reason';

	let tradeId = $derived(Number($page.params.id));
	let detail = $state<TradeDetail | null>(null);
	let error = $state<string | null>(null);
	let isLoading = $state(true);
	let loadedId = $state<number | null>(null);

	$effect(() => {
		const id = tradeId;
		if (loadedId === id) return;
		loadedId = id;
		load(id);
	});

	async function load(id: number) {
		isLoading = true;
		error = null;
		detail = null;
		if (!isTauri()) { isLoading = false; return; }
		if (!Number.isFinite(id)) {
			error = 'Invalid trade id';
			isLoading = false;
			return;
		}
		try {
			detail = await getTradeDetail(id);
		} catch (e: any) {
			error = String(e?.message ?? e);
		} finally {
			isLoading = false;
		}
	}

	const DIMS: { key: keyof TradeSignalSnapshot; label: string }[] = [
		{ key: 'political', label: 'political / lobbying' },
		{ key: 'government', label: 'government contracts' },
		{ key: 'news', label: 'news momentum' },
		{ key: 'institutional', label: 'institutional flow' },
		{ key: 'insider', label: 'insider buying' },
		{ key: 'search', label: 'search trend' },
		{ key: 'patent', label: 'patent filings' },
		{ key: 'supply_chain', label: 'supply chain' }
	];

	let reason = $derived(detail ? parseTradeReason(detail.trade.signal_profile) : null);
	let isOpen = $derived(detail?.trade.status === 'open');
	// position_size is dollar notional; unrealized P&L scales it by price move.
	let livePnlPct = $derived(
		detail?.exit_plan && detail.exit_plan.current_price > 0 && detail.trade.entry_price > 0
			? (detail.exit_plan.current_price / detail.trade.entry_price - 1) * 100
			: null
	);
	let livePnlDollars = $derived(
		livePnlPct !== null && detail ? detail.trade.position_size * (livePnlPct / 100) : null
	);

	function fmtDateTime(s: string | null | undefined): string {
		if (!s) return '—';
		const [d, t] = s.split('T');
		return t ? `${d} at ${t}` : d;
	}
	function fmt$(v: number | null | undefined, digits = 2): string {
		if (v === null || v === undefined) return '—';
		return '$' + v.toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits });
	}
	function distPct(level: number | null | undefined, current: number | undefined): string {
		if (level === null || level === undefined || !current || current <= 0) return '';
		const d = (level / current - 1) * 100;
		return `${d >= 0 ? '+' : ''}${d.toFixed(1)}% from current`;
	}
	function dim(s: TradeSignalSnapshot, key: keyof TradeSignalSnapshot): number {
		const v = s[key];
		return typeof v === 'number' ? v : 0;
	}
</script>

<div class="max-w-3xl mx-auto px-6 py-8">
	<a href="/signals" class="text-xs text-text-muted hover:text-text transition-colors">&larr; Back to Signals</a>

	{#if isLoading}
		<p class="mt-8 text-sm text-text-muted">Loading trade…</p>
	{:else if error}
		<p class="mt-8 text-sm text-rose-400">{error}</p>
	{:else if !detail}
		<p class="mt-8 text-sm text-text-muted">Trade not found.</p>
	{:else}
		{@const t = detail.trade}
		{@const plan = detail.exit_plan}
		<!-- Header -->
		<div class="mt-4 flex items-start justify-between">
			<div>
				<div class="flex items-center gap-3">
					<h1 class="text-2xl font-mono font-bold text-text">{t.ticker}</h1>
					<span class="text-[10px] px-1.5 py-0.5 rounded font-medium {
						t.status === 'open' ? 'bg-blue-500/15 text-blue-400' :
						t.status === 'stopped_out' ? 'bg-rose-500/15 text-rose-400' :
						'bg-zinc-500/15 text-zinc-400'
					}">{t.status}</span>
					{#if reason?.kind === 'auto'}
						<span class="text-[10px] px-1.5 py-0.5 rounded font-medium bg-indigo-500/15 text-indigo-400">auto</span>
					{:else if reason?.kind === 'manual'}
						<span class="text-[10px] px-1.5 py-0.5 rounded font-medium bg-zinc-500/15 text-zinc-400">manual</span>
					{:else if reason?.kind === 'reconciled'}
						<span class="text-[10px] px-1.5 py-0.5 rounded font-medium bg-zinc-500/15 text-zinc-400">imported</span>
					{/if}
				</div>
				{#if detail.entity_name}
					<p class="mt-1 text-sm text-text-muted">{detail.entity_name}</p>
				{/if}
			</div>
			<div class="text-right">
				{#if isOpen && livePnlPct !== null}
					<span class="text-xl font-mono font-bold {livePnlPct >= 0 ? 'text-emerald-400' : 'text-rose-400'}">
						{livePnlPct >= 0 ? '+' : ''}{livePnlPct.toFixed(1)}%
					</span>
					{#if livePnlDollars !== null}
						<p class="text-xs font-mono {livePnlDollars >= 0 ? 'text-emerald-400/70' : 'text-rose-400/70'}">
							{livePnlDollars >= 0 ? '+' : ''}{fmt$(livePnlDollars)}
						</p>
					{/if}
				{:else if t.pnl_pct !== null && t.pnl_pct !== undefined}
					<span class="text-xl font-mono font-bold {t.pnl_pct >= 0 ? 'text-emerald-400' : 'text-rose-400'}">
						{t.pnl_pct >= 0 ? '+' : ''}{t.pnl_pct.toFixed(1)}%
					</span>
				{/if}
			</div>
		</div>

		<!-- Entry -->
		<div class="mt-6 bg-bg-card border border-border rounded-xl p-5">
			<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Entry</h2>
			<div class="grid grid-cols-2 gap-x-6 gap-y-2 text-xs">
				<span class="text-text-muted">Opened (local time)</span>
				<span class="text-text font-mono">{fmtDateTime(t.entry_date)}</span>
				<span class="text-text-muted">Recorded in DB (UTC)</span>
				<span class="text-text font-mono">{detail.created_at ?? '—'}</span>
				<span class="text-text-muted">Fill price</span>
				<span class="text-text font-mono">{fmt$(t.entry_price)}</span>
				<span class="text-text-muted">Position (dollar notional)</span>
				<span class="text-text font-mono">{fmt$(t.position_size, 0)}</span>
				{#if detail.alpaca_order_id}
					<span class="text-text-muted">Alpaca order</span>
					<span class="text-text font-mono truncate" title={detail.alpaca_order_id}>{detail.alpaca_order_id}</span>
				{/if}
			</div>
			<p class="mt-3 text-xs text-text-secondary leading-relaxed">
				{#if reason?.kind === 'auto'}
					Placed automatically by the daily fetcher pipeline (Phase 13.5).
					{#if detail.entry_signals?.computed_at}
						Convergence was computed on <span class="font-mono">{detail.entry_signals.computed_at.slice(0, 10)}</span>
						with {detail.entry_signals.source_diversity} independent source type{detail.entry_signals.source_diversity === 1 ? '' : 's'} aligned
						and compound score <span class="font-mono">{detail.entry_signals.compound_score.toFixed(2)}</span> above the 0.30 entry gate.
					{/if}
					The buy fired on the first scheduled pipeline run after detection (slots at 7:00, 12:00, 18:00, 22:00) —
					the fill time is run start plus pipeline duration, not a chosen market timing.
				{:else if reason?.kind === 'manual'}
					Opened manually from the Signals page.
				{:else if reason?.kind === 'reconciled'}
					Imported from Alpaca during ledger reconciliation — Pulse did not originate this position, so entry
					signal context was not recorded.
				{:else}
					No entry context was recorded for this trade.
				{/if}
			</p>
		</div>

		<!-- Why this trade -->
		{#if reason?.kind === 'auto'}
			{@const sz = detail.sizing}
			<div class="mt-4 bg-bg-card border border-border rounded-xl p-5">
				<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Why this trade</h2>
				{#if detail.entry_signals}
					{@const es = detail.entry_signals}
					<div class="space-y-1.5">
						{#each DIMS as d}
							{@const v = dim(es, d.key)}
							{#if v > 0.001}
								<div class="flex items-center gap-3 text-xs">
									<span class="w-40 shrink-0 text-text-muted">{d.label}</span>
									<div class="flex-1 h-1.5 bg-border rounded-full overflow-hidden">
										<div class="h-full rounded-full bg-indigo-400/80" style="width: {Math.min(v * 100, 100)}%"></div>
									</div>
									<span class="w-10 text-right font-mono text-text-secondary">{(v * 100).toFixed(0)}%</span>
								</div>
							{/if}
						{/each}
					</div>
				{:else if reason.signals.length > 0}
					<div class="space-y-1.5">
						{#each reason.signals as s}
							<div class="flex items-center gap-3 text-xs">
								<span class="w-40 shrink-0 text-text-muted">{s.label}</span>
								<div class="flex-1 h-1.5 bg-border rounded-full overflow-hidden">
									<div class="h-full rounded-full bg-indigo-400/80" style="width: {Math.min(s.score * 100, 100)}%"></div>
								</div>
								<span class="w-10 text-right font-mono text-text-secondary">{(s.score * 100).toFixed(0)}%</span>
							</div>
						{/each}
					</div>
				{/if}
				{#if reason.stories.length > 0}
					<div class="mt-4">
						<p class="text-[11px] text-text-muted mb-1.5">Headlines captured at entry:</p>
						<ul class="space-y-1">
							{#each reason.stories as story}
								<li class="text-xs text-text-secondary">
									&ldquo;{story.title}&rdquo;{#if story.source}<span class="text-text-muted"> — {story.source}</span>{/if}
								</li>
							{/each}
						</ul>
					</div>
				{:else}
					<p class="mt-4 text-[11px] text-text-muted">
						No specific headlines were captured at entry — the drivers above came from structured data
						(lobbying filings, government contracts, filings) rather than news stories.
					</p>
				{/if}
				<p class="mt-4 text-[11px] text-text-muted leading-relaxed border-t border-border/50 pt-3">
					Entry rule: convergence (2+ signals above 0.30 with source diversity ≥ 2, or compound ≥ 0.40),
					compound score &gt; 0.30, no existing open position in the ticker, and no heavy insider selling
					(net insider sales beyond −$1M veto the trade).
				</p>
			</div>

			<!-- Why this amount -->
			<div class="mt-4 bg-bg-card border border-border rounded-xl p-5">
				<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Why this amount</h2>
				<p class="text-xs text-text-secondary leading-relaxed">
					Compound score <span class="font-mono">{sz.score.toFixed(2)}</span> lands in the
					<span class="font-mono">{(sz.tier_pct * 100).toFixed(0)}%</span> tier
					(&gt;0.60 &rarr; 5%, &gt;0.40 &rarr; 2%, otherwise 1% of buying power).
					{#if sz.implied_buying_power !== null}
						{(sz.tier_pct * 100).toFixed(0)}% of the {fmt$(sz.implied_buying_power, 0)} buying power
						available at entry = <span class="font-mono text-text">{fmt$(sz.notional, 0)}</span>.
					{:else}
						Result: <span class="font-mono text-text">{fmt$(sz.notional, 0)}</span>{sz.clamped ? ' (clamped by the floor/cap bounds)' : ''}.
					{/if}
					Bounds: floor {fmt$(sz.entry_floor, 0)}, cap {fmt$(sz.entry_cap, 0)} per entry.
					{#if sz.scale_in_count > 0}
						Includes {sz.scale_in_count} scale-in{sz.scale_in_count === 1 ? '' : 's'} (1% of buying power each, added when the signal strengthened ≥ 1.2× while the position was green).
					{/if}
				</p>
			</div>
		{/if}

		<!-- Exit plan -->
		{#if isOpen && plan}
			<div class="mt-4 bg-bg-card border border-border rounded-xl p-5">
				<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Exit plan — live levels</h2>
				<p class="text-xs text-text-muted mb-3">
					Current price <span class="font-mono text-text">{plan.current_price > 0 ? fmt$(plan.current_price) : '— (no local price data)'}</span>
					{#if plan.price_date && plan.current_price > 0}<span class="text-text-muted"> (as of {plan.price_date})</span>{/if}
					&middot; held {plan.days_held} day{plan.days_held === 1 ? '' : 's'}
					&middot; high-water mark <span class="font-mono">{fmt$(plan.high_water_mark ?? t.entry_price)}</span>
				</p>

				{#if plan.no_atr_fallback}
					<div class="mb-3 text-[11px] text-amber-400/90 bg-amber-500/10 border border-amber-500/20 rounded-lg px-3 py-2">
						No ATR price data for {t.ticker} — the engine falls back to a fixed −10% stop
						({fmt$(plan.fixed_stop_price)}) plus the 90-day expiry instead of ATR-based levels.
					</div>
				{/if}

				<div class="divide-y divide-border/50 text-xs">
					{#if !plan.no_atr_fallback && plan.live_trailing_stop !== null}
						<div class="py-2.5 grid grid-cols-[9rem_1fr] gap-x-4">
							<span class="text-rose-300 font-medium">Trailing stop</span>
							<span class="text-text-secondary leading-relaxed">
								<span class="font-mono text-text">{fmt$(plan.live_trailing_stop)}</span>
								<span class="text-text-muted"> ({distPct(plan.live_trailing_stop, plan.current_price)})</span>
								— closes ALL. High-water {fmt$(plan.high_water_mark ?? t.entry_price)} −
								ATR {fmt$(plan.atr)} × {plan.atr_mult.toFixed(1)}
								<span class="text-text-muted">(multiplier tightens with age: 2.0× under 30d, 1.5× from 30d, 1.0× from 60d)</span>
							</span>
						</div>
					{/if}
					{#if !plan.no_atr_fallback && plan.profit_target_price !== null}
						<div class="py-2.5 grid grid-cols-[9rem_1fr] gap-x-4">
							<span class="text-emerald-300 font-medium">Profit target</span>
							<span class="text-text-secondary leading-relaxed">
								<span class="font-mono text-text">{fmt$(plan.profit_target_price)}</span>
								<span class="text-text-muted"> ({distPct(plan.profit_target_price, plan.current_price)})</span>
								— closes HALF when price ≥ target AND P&amp;L ≥ +10%. Entry + 3× ATR.
								{#if plan.half_closed_at}
									<span class="text-emerald-400/80">Half already closed on {plan.half_closed_at.slice(0, 10)}.</span>
								{/if}
							</span>
						</div>
					{/if}
					<div class="py-2.5 grid grid-cols-[9rem_1fr] gap-x-4">
						<span class="text-rose-300 font-medium">Hard stop</span>
						<span class="text-text-secondary leading-relaxed">
							<span class="font-mono text-text">{fmt$(plan.hard_stop_price)}</span>
							<span class="text-text-muted"> ({distPct(plan.hard_stop_price, plan.current_price)})</span>
							— −15% safety net, closes ALL regardless of ATR.
						</span>
					</div>
					<div class="py-2.5 grid grid-cols-[9rem_1fr] gap-x-4">
						<span class="text-text-muted font-medium">Max hold</span>
						<span class="text-text-secondary leading-relaxed">
							<span class="font-mono text-text">{plan.max_hold_date ?? '—'}</span>
							— {plan.days_remaining} of 90 days remain, then closes ALL.
						</span>
					</div>
					<div class="py-2.5 grid grid-cols-[9rem_1fr] gap-x-4">
						<span class="text-text-muted font-medium">Signal decay</span>
						<span class="text-text-secondary leading-relaxed">
							Closes ALL if the compound score falls below
							<span class="font-mono text-text">{plan.decay_threshold.toFixed(2)}</span>
							(30% of the {plan.decay_original_score.toFixed(2)} entry score, min 0.05).
							Latest score: <span class="font-mono {plan.decay_triggered ? 'text-rose-400' : 'text-text'}">{plan.decay_current_score.toFixed(2)}</span>{plan.decay_triggered ? ' — below threshold, exit expected on the next run' : ''}.
						</span>
					</div>
				</div>

				<p class="mt-3 text-[11px] text-text-muted leading-relaxed border-t border-border/50 pt-3">
					Exits are evaluated once per pipeline run (4 scheduled slots/day), not tick-by-tick — a level can be
					crossed intraday before the engine reacts. TP/SL is a price condition, not a scheduled time.
				</p>
			</div>
		{/if}

		<!-- Signals then vs now -->
		{#if isOpen && detail.entry_signals && detail.current_signals && detail.current_signals.computed_at !== detail.entry_signals.computed_at}
			{@const es = detail.entry_signals}
			{@const cs = detail.current_signals}
			<div class="mt-4 bg-bg-card border border-border rounded-xl p-5">
				<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">
					Signals: entry vs now
					<span class="normal-case font-normal">({es.computed_at?.slice(0, 10)} &rarr; {cs.computed_at?.slice(0, 10)})</span>
				</h2>
				<div class="space-y-1 text-xs">
					{#each DIMS as d}
						{@const then = dim(es, d.key)}
						{@const now = dim(cs, d.key)}
						{#if then > 0.001 || now > 0.001}
							<div class="flex items-center gap-3">
								<span class="w-40 shrink-0 text-text-muted">{d.label}</span>
								<span class="font-mono text-text-secondary">{(then * 100).toFixed(0)}% &rarr; {(now * 100).toFixed(0)}%</span>
								{#if now < then * 0.5}<span class="text-[10px] text-rose-400/80">fading</span>{/if}
							</div>
						{/if}
					{/each}
					<div class="flex items-center gap-3 pt-1.5 border-t border-border/50 mt-1.5">
						<span class="w-40 shrink-0 text-text-muted">compound</span>
						<span class="font-mono text-text">{es.compound_score.toFixed(2)} &rarr; {cs.compound_score.toFixed(2)}</span>
					</div>
				</div>
			</div>
		{/if}

		<!-- Journal (closed trades) -->
		{#if !isOpen}
			<div class="mt-4 bg-bg-card border border-border rounded-xl p-5">
				<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Exit</h2>
				<div class="grid grid-cols-2 gap-x-6 gap-y-2 text-xs mb-3">
					<span class="text-text-muted">Closed</span>
					<span class="text-text font-mono">{fmtDateTime(t.exit_date)}</span>
					<span class="text-text-muted">Exit price</span>
					<span class="text-text font-mono">{fmt$(t.exit_price)}</span>
					{#if t.pnl !== null && t.pnl !== undefined}
						<span class="text-text-muted">Realized P&amp;L</span>
						<span class="font-mono {t.pnl >= 0 ? 'text-emerald-400' : 'text-rose-400'}">{t.pnl >= 0 ? '+' : ''}{fmt$(t.pnl)}</span>
					{/if}
				</div>
				{#if t.trade_journal}
					<p class="text-xs text-text-secondary leading-relaxed bg-bg border border-border rounded-lg p-3">{t.trade_journal}</p>
				{/if}
			</div>
		{/if}
	{/if}
</div>
