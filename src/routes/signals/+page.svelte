<script lang="ts">
	import { isTauri } from '$lib/tauri/mock';
	import {
		getCrossSignals, getConvergenceAlerts, getEntityPrices, getPortfolio,
		executeTrade, getFinancialEvents, getSignalEvidence, getSourceHealth,
		getFinancialQuotas, refreshPrices
	} from '$lib/tauri/commands';
	import type {
		CrossSignal, EntityPrice, Portfolio, FinancialEvent,
		SignalEvidence, SourceHealth, FinancialApiQuota
	} from '$lib/tauri/types';

	let signals = $state<CrossSignal[]>([]);
	let convergence = $state<CrossSignal[]>([]);
	let evidence = $state<SignalEvidence[]>([]);
	let prices = $state<EntityPrice[]>([]);
	let events = $state<FinancialEvent[]>([]);
	let portfolio = $state<Portfolio | null>(null);
	let sources = $state<SourceHealth[]>([]);
	let quotas = $state<FinancialApiQuota[]>([]);
	let isLoading = $state(true);
	let error = $state<string | null>(null);
	let activeTab = $state<'overview' | 'market' | 'portfolio' | 'sources'>('overview');
	let loaded = $state(false);
	let expandedSignal = $state<number | null>(null);
	let tradingTicker = $state('');
	let tradingConfidence = $state(0.6);
	let tradeStatus = $state<string | null>(null);
	let pricesRefreshing = $state(false);

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadData();
	});

	async function loadData() {
		error = null;
		isLoading = true;
		try {
			if (!isTauri()) { isLoading = false; return; }
			const [s, c, ev, e, p] = await Promise.all([
				getCrossSignals(30).catch(() => []),
				getConvergenceAlerts(10).catch(() => []),
				getSignalEvidence(15).catch(() => []),
				getFinancialEvents(20).catch(() => []),
				getEntityPrices(50).catch(() => []),
			]);
			signals = s;
			convergence = c;
			evidence = ev;
			events = e;
			prices = p;
			// Load async data separately (Alpaca API call + price refresh)
			getPortfolio().then(pf => { portfolio = pf; }).catch(() => {});
			getSourceHealth().then(sh => { sources = sh; }).catch(() => {});
			getFinancialQuotas().then(q => { quotas = q; }).catch(() => {});
			// Refresh prices in background, then reload price list
			pricesRefreshing = true;
			refreshPrices()
				.then(() => getEntityPrices(50))
				.then(p => { prices = p; })
				.catch(() => {})
				.finally(() => { pricesRefreshing = false; });
		} catch (e: any) {
			error = String(e?.message ?? e);
		} finally {
			isLoading = false;
		}
	}

	async function handleTrade(ticker: string, confidence: number) {
		tradeStatus = `Placing order for ${ticker}...`;
		try {
			const result = await executeTrade(ticker, confidence);
			if (result) {
				tradeStatus = `Bought ${result.ticker} @ $${result.entry_price.toFixed(2)} — $${result.position_size.toFixed(0)} position`;
			} else {
				tradeStatus = 'Skipped (already holding or max positions)';
			}
			getPortfolio().then(pf => { portfolio = pf; }).catch(() => {});
		} catch (e: any) {
			tradeStatus = `Error: ${e?.message ?? e}`;
		}
	}

	function changeColor(val: number | null): string {
		if (val === null) return 'text-text-muted';
		return val >= 0 ? 'text-emerald-400' : 'text-rose-400';
	}

	function fmtChange(val: number | null): string {
		if (val === null) return '--';
		return `${val >= 0 ? '+' : ''}${val.toFixed(1)}%`;
	}

	function fmtPrice(val: number): string {
		return val.toLocaleString('en-US', { style: 'currency', currency: 'USD' });
	}

	function fmtPnl(val: number): string {
		return `${val >= 0 ? '+' : ''}${fmtPrice(val)}`;
	}

	function sourceColor(name: string): string {
		if (name.includes('SEC') || name.includes('EDGAR')) return 'text-blue-400';
		if (name.includes('USASpending')) return 'text-amber-400';
		if (name.includes('Federal')) return 'text-rose-400';
		if (name.includes('FRED')) return 'text-cyan-400';
		if (name.includes('FEC')) return 'text-emerald-400';
		if (name.includes('EIA')) return 'text-orange-400';
		if (name.includes('LDA') || name.includes('Lobby')) return 'text-purple-400';
		if (name.includes('Finnhub')) return 'text-sky-400';
		if (name.includes('Alpaca')) return 'text-yellow-400';
		return 'text-text-muted';
	}

	function statusDot(status: string): string {
		if (status === 'active') return 'bg-emerald-400';
		if (status === 'migrating') return 'bg-amber-400';
		return 'bg-zinc-600';
	}

	function recColor(rec: string): string {
		if (rec.startsWith('Strong Buy')) return 'text-emerald-400 bg-emerald-500/10 border-emerald-500/20';
		if (rec.startsWith('Buy')) return 'text-blue-400 bg-blue-500/10 border-blue-500/20';
		if (rec.startsWith('Watch')) return 'text-amber-400 bg-amber-500/10 border-amber-500/20';
		if (rec.startsWith('Monitor')) return 'text-zinc-400 bg-zinc-500/10 border-zinc-500/20';
		return 'text-zinc-500 bg-zinc-500/5 border-zinc-700/30';
	}

	function recIcon(rec: string): string {
		if (rec.startsWith('Strong Buy')) return '▲▲';
		if (rec.startsWith('Buy')) return '▲';
		if (rec.startsWith('Watch')) return '◉';
		if (rec.startsWith('Monitor')) return '○';
		return '·';
	}

	const activeSources = $derived(sources.filter(s => s.status === 'active').length);
	const totalEvents = $derived(events.length);
	const openTrades = $derived(portfolio?.open_trades.length ?? 0);

	// Portfolio P&L
	const totalPnl = $derived(
		portfolio?.positions.reduce((sum, p) => sum + p.unrealized_pl, 0) ?? 0
	);
	const totalPnlPct = $derived(
		portfolio && portfolio.portfolio_value > 0
			? (totalPnl / (portfolio.portfolio_value - totalPnl)) * 100
			: 0
	);

	// Actionable signals (Buy or Strong Buy only)
	const actionable = $derived(evidence.filter(e =>
		e.recommendation.startsWith('Strong Buy') || e.recommendation.startsWith('Buy')
	));

	// Keyboard shortcuts
	function handleKeydown(e: KeyboardEvent) {
		if (e.target instanceof HTMLInputElement) return;
		if (e.key === '1') activeTab = 'overview';
		if (e.key === '2') activeTab = 'market';
		if (e.key === '3') activeTab = 'portfolio';
		if (e.key === '4') activeTab = 'sources';
		if (e.key === 'r') loadData();
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="max-w-5xl mx-auto px-6 py-6">
	<!-- Header -->
	<div class="flex items-center justify-between mb-5">
		<div>
			<h1 class="text-xl font-bold text-text">Signals</h1>
			<p class="text-xs text-text-muted mt-0.5">
				{activeSources} sources active · {totalEvents} events · {signals.length} entities scored
				{#if pricesRefreshing}
					<span class="text-ai ml-1">· refreshing prices...</span>
				{/if}
			</p>
		</div>
		{#if convergence.length > 0}
			<div class="flex items-center gap-2 px-3 py-1.5 bg-emerald-500/10 border border-emerald-500/20 rounded-lg">
				<div class="w-2 h-2 rounded-full bg-emerald-400 animate-pulse"></div>
				<span class="text-emerald-400 text-sm font-medium">{convergence.length} convergence</span>
			</div>
		{/if}
	</div>

	<!-- Stats strip -->
	<div class="grid grid-cols-4 gap-3 mb-5">
		<div class="bg-bg-card border border-border rounded-xl p-3">
			<div class="text-[10px] text-text-muted uppercase tracking-wider">Top Score</div>
			<div class="text-xl font-mono font-bold text-text">{signals.length > 0 ? `${(signals[0]?.compound_score * 100).toFixed(0)}%` : '--'}</div>
		</div>
		<div class="bg-bg-card border border-border rounded-xl p-3">
			<div class="text-[10px] text-text-muted uppercase tracking-wider">Actionable</div>
			<div class="text-xl font-mono font-bold {actionable.length > 0 ? 'text-emerald-400' : 'text-text'}">{actionable.length} signals</div>
		</div>
		<div class="bg-bg-card border border-border rounded-xl p-3">
			<div class="text-[10px] text-text-muted uppercase tracking-wider">Portfolio P&L</div>
			<div class="text-xl font-mono font-bold {totalPnl >= 0 ? 'text-emerald-400' : 'text-rose-400'}">
				{portfolio ? fmtPnl(totalPnl) : '--'}
			</div>
		</div>
		<div class="bg-bg-card border border-border rounded-xl p-3">
			<div class="text-[10px] text-text-muted uppercase tracking-wider">Positions</div>
			<div class="text-xl font-mono font-bold text-text">{openTrades} open</div>
		</div>
	</div>

	<!-- Tab bar -->
	<div class="flex gap-1 mb-5 bg-bg-card rounded-lg p-1">
		{#each [
			{ id: 'overview', label: 'Overview', key: '1' },
			{ id: 'market', label: 'Market', key: '2' },
			{ id: 'portfolio', label: 'Portfolio', key: '3' },
			{ id: 'sources', label: 'Sources', key: '4' },
		] as tab}
			<button
				class="flex-1 py-2 px-3 text-sm rounded-md transition-colors {activeTab === tab.id ? 'bg-bg text-text font-medium shadow-sm' : 'text-text-muted hover:text-text'}"
				onclick={() => { activeTab = tab.id as any; }}
			>
				{tab.label} <span class="text-[10px] text-text-muted ml-0.5">{tab.key}</span>
			</button>
		{/each}
	</div>

	{#if isLoading}
		<div class="flex items-center justify-center py-20">
			<div class="w-6 h-6 border-2 border-ai border-t-transparent rounded-full animate-spin"></div>
		</div>

	{:else if error}
		<div class="text-center py-20 text-rose-400">{error}</div>

	<!-- ==================== OVERVIEW TAB ==================== -->
	{:else if activeTab === 'overview'}

		{#if tradeStatus}
			<div class="mb-4 px-4 py-2.5 rounded-lg text-sm {tradeStatus.startsWith('Error') ? 'bg-rose-500/10 text-rose-400 border border-rose-500/20' : 'bg-emerald-500/10 text-emerald-400 border border-emerald-500/20'}">
				{tradeStatus}
			</div>
		{/if}

		<!-- Actionable Signals (Buy/Strong Buy) -->
		{#if actionable.length > 0}
			<h2 class="text-xs font-semibold text-emerald-400 uppercase tracking-wider mb-3">Action Required</h2>
			<div class="space-y-2 mb-6">
				{#each actionable as ev}
					<div class="bg-bg-card border border-emerald-500/20 rounded-xl px-5 py-4 flex items-center justify-between">
						<div class="flex items-center gap-3">
							<span class="text-emerald-400 text-lg font-bold">{recIcon(ev.recommendation)}</span>
							<div>
								<div class="flex items-center gap-2">
									<span class="font-semibold text-text">{ev.entity_name}</span>
									{#if ev.ticker}
										<span class="text-[10px] font-mono text-text-muted bg-bg px-1.5 py-0.5 rounded">{ev.ticker}</span>
									{/if}
									{#if ev.price}
										<span class="text-sm font-mono text-text-secondary">{fmtPrice(ev.price)}</span>
									{/if}
								</div>
								<p class="text-xs text-text-muted mt-0.5">{ev.reasons[0] ?? ''}</p>
							</div>
						</div>
						<div class="flex items-center gap-3">
							<span class="text-sm font-mono font-bold text-emerald-400">{(ev.compound_score * 100).toFixed(0)}%</span>
							{#if ev.ticker}
								<button
									class="px-4 py-1.5 bg-emerald-500/15 text-emerald-400 border border-emerald-500/25 rounded-lg text-xs font-semibold hover:bg-emerald-500/25 transition-colors"
									onclick={() => handleTrade(ev.ticker!, ev.compound_score)}
								>
									Buy {ev.ticker}
								</button>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}

		<!-- All Signal Evidence -->
		{#if evidence.length > 0}
			<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">All Signals</h2>
			<div class="space-y-2 mb-6">
				{#each evidence as ev, idx}
					{@const isExpanded = expandedSignal === idx}
					<div class="bg-bg-card border {ev.recommendation.startsWith('Strong Buy') ? 'border-emerald-500/25' : ev.recommendation.startsWith('Buy') ? 'border-blue-500/20' : 'border-border'} rounded-xl overflow-hidden">
						<button class="w-full px-5 py-3 text-left" onclick={() => { expandedSignal = isExpanded ? null : idx; }}>
							<div class="flex items-center justify-between">
								<div class="flex items-center gap-2.5">
									<span class="text-xs w-5 text-center {ev.recommendation.startsWith('Strong') ? 'text-emerald-400' : ev.recommendation.startsWith('Buy') ? 'text-blue-400' : ev.recommendation.startsWith('Watch') ? 'text-amber-400' : 'text-zinc-500'}">{recIcon(ev.recommendation)}</span>
									<span class="text-sm font-semibold text-text">{ev.entity_name}</span>
									{#if ev.ticker}
										<span class="text-[10px] font-mono text-text-muted bg-bg px-1.5 py-0.5 rounded">{ev.ticker}</span>
									{/if}
									{#if ev.price}
										<span class="text-xs font-mono text-text-secondary">{fmtPrice(ev.price)}</span>
										{#if ev.price_change_1d !== null}
											<span class="text-[10px] font-mono {changeColor(ev.price_change_1d)}">{fmtChange(ev.price_change_1d)}</span>
										{/if}
									{/if}
								</div>
								<div class="flex items-center gap-3">
									<span class="inline-flex items-center px-2 py-0.5 rounded text-[10px] font-medium border {recColor(ev.recommendation)}">
										{ev.recommendation.split(' — ')[0]}
									</span>
									<span class="text-sm font-mono font-bold {ev.compound_score > 0.4 ? 'text-emerald-400' : 'text-text-secondary'}">{(ev.compound_score * 100).toFixed(0)}%</span>
									<span class="text-text-muted text-xs">{isExpanded ? '−' : '+'}</span>
								</div>
							</div>
						</button>

						{#if isExpanded}
							<div class="px-5 pb-4 border-t border-border pt-3 space-y-3">
								<div>
									<h4 class="text-[10px] font-semibold text-text-muted uppercase tracking-wider mb-1.5">Why</h4>
									<ul class="space-y-1">
										{#each ev.reasons as reason}
											<li class="text-sm text-text-secondary flex items-start gap-2">
												<span class="text-ai mt-0.5">+</span> {reason}
											</li>
										{/each}
									</ul>
								</div>
								{#if ev.source_stories.length > 0}
									<div>
										<h4 class="text-[10px] font-semibold text-text-muted uppercase tracking-wider mb-1.5">Evidence</h4>
										<div class="space-y-1">
											{#each ev.source_stories as story}
												<div class="flex items-start gap-2 text-xs">
													<span class="{sourceColor(story.source_name)}">+</span>
													<span class="text-text-secondary flex-1">{story.headline}</span>
													<span class="text-text-muted shrink-0">{story.source_name}</span>
												</div>
											{/each}
										</div>
									</div>
								{/if}
								<div class="flex items-center justify-between bg-bg rounded-lg px-4 py-3">
									<div class="text-xs text-text-muted">
										Position: <span class="text-text font-mono">{ev.position_size_pct}%</span>
										{#if portfolio}
											<span class="text-text-muted"> = {fmtPrice(portfolio.portfolio_value * ev.position_size_pct / 100)}</span>
										{/if}
									</div>
									{#if ev.ticker}
										<button
											class="px-4 py-1.5 bg-ai/10 text-ai border border-ai/25 rounded-lg text-xs font-medium hover:bg-ai/20 transition-colors"
											onclick={() => handleTrade(ev.ticker!, ev.compound_score)}
										>Buy {ev.ticker}</button>
									{/if}
								</div>
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{:else}
			<div class="bg-bg-card border border-border rounded-xl p-8 text-center mb-6">
				<p class="text-base text-text-secondary mb-2">No signal intelligence yet</p>
				<p class="text-sm text-text-muted">Run the pipeline daily to accumulate financial data. Signals appear when entities are mentioned across multiple source types.</p>
			</div>
		{/if}

		<!-- Recent Financial Events -->
		{#if events.length > 0}
			<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Recent Financial Events</h2>
			<div class="bg-bg-card border border-border rounded-xl divide-y divide-border/50">
				{#each events.slice(0, 12) as event}
					<div class="px-4 py-2.5 flex items-center gap-3">
						<span class="text-sm {sourceColor(event.source_name)}">+</span>
						<span class="text-[10px] font-semibold uppercase w-10 shrink-0 {sourceColor(event.source_name)}">
							{event.source_name.split(' ')[0].slice(0, 5)}
						</span>
						<span class="text-sm text-text flex-1 truncate">{event.headline}</span>
						{#if event.published_at}
							<span class="text-[10px] text-text-muted shrink-0">{event.published_at.slice(0, 10)}</span>
						{/if}
					</div>
				{/each}
			</div>
		{/if}

	<!-- ==================== MARKET TAB ==================== -->
	{:else if activeTab === 'market'}

		{#if prices.length === 0}
			<div class="bg-bg-card border border-border rounded-xl p-8 text-center">
				<p class="text-base text-text-secondary mb-2">No price data yet</p>
				<p class="text-sm text-text-muted">Prices are fetched daily for entities with ticker mappings.</p>
			</div>
		{:else}
			<div class="bg-bg-card border border-border rounded-xl overflow-hidden">
				<div class="grid grid-cols-6 px-4 py-2.5 text-[10px] text-text-muted uppercase tracking-wider border-b border-border">
					<span class="col-span-2">Entity</span>
					<span class="text-right">Price</span>
					<span class="text-right">1D</span>
					<span class="text-right">7D</span>
					<span class="text-right">30D</span>
				</div>
				{#each prices as p}
					<div class="grid grid-cols-6 px-4 py-2.5 items-center hover:bg-bg-card-hover transition-colors border-b border-border/50 last:border-0">
						<div class="col-span-2 flex items-center gap-2">
							<span class="text-sm font-mono font-semibold text-text">{p.ticker}</span>
							{#if p.entity_name}
								<span class="text-[10px] text-text-muted truncate">{p.entity_name}</span>
							{/if}
						</div>
						<span class="text-sm font-mono text-text text-right">${p.close.toFixed(2)}</span>
						<span class="text-sm font-mono text-right {changeColor(p.change_1d)}">{fmtChange(p.change_1d)}</span>
						<span class="text-sm font-mono text-right {changeColor(p.change_7d)}">{fmtChange(p.change_7d)}</span>
						<span class="text-sm font-mono text-right {changeColor(p.change_30d)}">{fmtChange(p.change_30d)}</span>
					</div>
				{/each}
			</div>
		{/if}

	<!-- ==================== PORTFOLIO TAB ==================== -->
	{:else if activeTab === 'portfolio'}

		{#if portfolio}
			<!-- P&L Hero -->
			<div class="bg-bg-card border {totalPnl >= 0 ? 'border-emerald-500/20' : 'border-rose-500/20'} rounded-xl p-6 mb-5 text-center">
				<div class="text-[10px] text-text-muted uppercase tracking-wider mb-1">Total Unrealized P&L</div>
				<div class="text-3xl font-mono font-bold {totalPnl >= 0 ? 'text-emerald-400' : 'text-rose-400'}">
					{fmtPnl(totalPnl)}
				</div>
				<div class="text-sm font-mono {totalPnlPct >= 0 ? 'text-emerald-400/70' : 'text-rose-400/70'}">
					{totalPnlPct >= 0 ? '+' : ''}{totalPnlPct.toFixed(2)}%
				</div>
			</div>

			<!-- Summary row -->
			<div class="grid grid-cols-3 gap-3 mb-5">
				<div class="bg-bg-card border border-border rounded-xl p-3">
					<div class="text-[10px] text-text-muted uppercase tracking-wider">Equity</div>
					<div class="text-lg font-mono font-bold text-text">{fmtPrice(portfolio.equity)}</div>
				</div>
				<div class="bg-bg-card border border-border rounded-xl p-3">
					<div class="text-[10px] text-text-muted uppercase tracking-wider">Cash</div>
					<div class="text-lg font-mono font-bold text-text">{fmtPrice(portfolio.cash)}</div>
				</div>
				<div class="bg-bg-card border border-border rounded-xl p-3">
					<div class="text-[10px] text-text-muted uppercase tracking-wider">Buying Power</div>
					<div class="text-lg font-mono font-bold text-text">{fmtPrice(portfolio.buying_power)}</div>
				</div>
			</div>

			<!-- Open Positions -->
			{#if portfolio.positions.length > 0}
				<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Open Positions</h2>
				<div class="space-y-2 mb-6">
					{#each portfolio.positions as pos}
						{@const pnlPct = pos.unrealized_pl_pct * 100}
						{@const isWinning = pos.unrealized_pl >= 0}
						<div class="bg-bg-card border {isWinning ? 'border-emerald-500/15' : 'border-rose-500/15'} rounded-xl px-5 py-4">
							<div class="flex items-center justify-between mb-2">
								<div class="flex items-center gap-3">
									<span class="text-sm font-mono font-bold text-text">{pos.symbol}</span>
									<span class="text-xs text-text-muted">{pos.qty.toFixed(2)} shares</span>
								</div>
								<div class="text-right">
									<span class="text-lg font-mono font-bold {isWinning ? 'text-emerald-400' : 'text-rose-400'}">
										{fmtPnl(pos.unrealized_pl)}
									</span>
									<span class="text-xs font-mono ml-1.5 {isWinning ? 'text-emerald-400/70' : 'text-rose-400/70'}">
										{pnlPct >= 0 ? '+' : ''}{pnlPct.toFixed(1)}%
									</span>
								</div>
							</div>
							<div class="flex items-center justify-between text-xs text-text-muted">
								<span>Entry: ${pos.avg_entry_price.toFixed(2)} &rarr; Now: ${pos.current_price.toFixed(2)}</span>
								<span>{fmtPrice(pos.market_value)} value</span>
							</div>
							<!-- P&L bar -->
							<div class="mt-2 h-1 bg-border rounded-full overflow-hidden">
								{@const barWidth = Math.min(Math.abs(pnlPct) * 5, 100)}
								<div class="h-full rounded-full {isWinning ? 'bg-emerald-400' : 'bg-rose-400'}" style="width: {barWidth}%"></div>
							</div>
						</div>
					{/each}
				</div>
			{/if}

			<!-- Trade History from DB (with live P&L for open trades) -->
			{#if portfolio.open_trades.length > 0 || portfolio.closed_trades.length > 0}
				<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Trade Log</h2>
				<div class="bg-bg-card border border-border rounded-xl divide-y divide-border/50 mb-6">
					{#each [...portfolio.open_trades, ...portfolio.closed_trades] as trade}
						{@const hasPnl = trade.pnl_pct !== null && trade.pnl_pct !== undefined}
						{@const pnl = trade.pnl_pct ?? 0}
						{@const isGreen = pnl >= 0}
						<div class="px-4 py-3 flex items-center justify-between">
							<div class="flex items-center gap-2.5">
								<span class="w-2 h-2 rounded-full {trade.status === 'open' ? (isGreen ? 'bg-emerald-400' : 'bg-rose-400') : 'bg-zinc-500'}"></span>
								<span class="text-sm font-mono font-semibold text-text">{trade.ticker}</span>
								<span class="text-xs text-text-muted">@ ${trade.entry_price.toFixed(2)}</span>
								<span class="text-[10px] text-text-muted">{trade.entry_date.slice(0, 10)}</span>
							</div>
							<div class="flex items-center gap-2">
								{#if hasPnl}
									<span class="text-sm font-mono font-bold {isGreen ? 'text-emerald-400' : 'text-rose-400'}">
										{isGreen ? '+' : ''}{pnl.toFixed(1)}%
									</span>
									{#if trade.pnl !== null && trade.pnl !== undefined}
										<span class="text-xs font-mono {isGreen ? 'text-emerald-400/60' : 'text-rose-400/60'}">
											({fmtPnl(trade.pnl)})
										</span>
									{/if}
								{:else}
									<span class="text-xs text-text-muted">pending</span>
								{/if}
								<span class="text-[10px] px-1.5 py-0.5 rounded font-medium {
									trade.status === 'open' ? 'bg-blue-500/15 text-blue-400' :
									trade.status === 'closed' && pnl > 0 ? 'bg-emerald-500/15 text-emerald-400' :
									trade.status === 'stopped_out' ? 'bg-rose-500/15 text-rose-400' :
									'bg-zinc-500/15 text-zinc-400'
								}">{trade.status}</span>
							</div>
						</div>
					{/each}
				</div>
			{/if}

			<!-- Manual trade -->
			<div class="bg-bg-card border border-border rounded-xl p-5">
				<h3 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Manual Paper Trade</h3>
				<div class="flex gap-3 items-end">
					<div class="flex-1">
						<label for="trade-ticker" class="text-[10px] text-text-muted block mb-1">Ticker</label>
						<input id="trade-ticker" type="text" bind:value={tradingTicker} placeholder="AAPL"
							class="w-full bg-bg border border-border rounded-lg px-3 py-2 text-sm text-text font-mono focus:outline-none focus:border-ai transition-colors" />
					</div>
					<div class="w-28">
						<label for="trade-confidence" class="text-[10px] text-text-muted block mb-1">Confidence</label>
						<input id="trade-confidence" type="number" bind:value={tradingConfidence} min="0.1" max="1.0" step="0.1"
							class="w-full bg-bg border border-border rounded-lg px-3 py-2 text-sm text-text font-mono focus:outline-none focus:border-ai transition-colors" />
					</div>
					<button
						onclick={() => handleTrade(tradingTicker.toUpperCase(), tradingConfidence)}
						class="px-5 py-2 bg-ai/10 text-ai border border-ai/25 rounded-lg text-sm font-medium hover:bg-ai/20 transition-colors"
					>Buy</button>
				</div>
				{#if tradeStatus}
					<p class="text-xs mt-2 {tradeStatus.startsWith('Error') ? 'text-rose-400' : 'text-emerald-400'}">{tradeStatus}</p>
				{/if}
			</div>
		{:else}
			<div class="bg-bg-card border border-border rounded-xl p-8 text-center">
				<div class="w-6 h-6 border-2 border-ai border-t-transparent rounded-full animate-spin mx-auto mb-3"></div>
				<p class="text-sm text-text-muted">Connecting to Alpaca...</p>
			</div>
		{/if}

	<!-- ==================== SOURCES TAB ==================== -->
	{:else if activeTab === 'sources'}

		<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Data Sources</h2>
		<div class="grid grid-cols-3 gap-3 mb-6">
			{#each sources as src}
				<div class="bg-bg-card border border-border rounded-xl p-4">
					<div class="flex items-center gap-2 mb-2">
						<div class="w-2 h-2 rounded-full {statusDot(src.status)}"></div>
						<span class="text-sm font-semibold {sourceColor(src.name)}">{src.name}</span>
					</div>
					<p class="text-xs text-text-muted mb-2">{src.description}</p>
					<div class="flex items-center justify-between text-[10px]">
						<span class="text-text-secondary font-mono">{src.last_count} items/week</span>
						<span class="{src.status === 'active' ? 'text-emerald-400' : src.status === 'migrating' ? 'text-amber-400' : 'text-zinc-500'}">
							{src.status === 'active' ? 'Active' : src.status === 'migrating' ? 'Migrating' : 'Inactive'}
						</span>
					</div>
				</div>
			{/each}
		</div>

		{#if quotas.length > 0}
			<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">API Rate Limits</h2>
			<div class="bg-bg-card border border-border rounded-xl divide-y divide-border/50">
				{#each quotas.filter(q => q.limit_per_minute > 0 || q.calls_today > 0) as q}
					{@const hourlyLimit = q.limit_per_minute > 0 ? q.limit_per_minute * 60 : 0}
					{@const pct = hourlyLimit > 0 ? Math.round((q.calls_this_hour / hourlyLimit) * 100) : 0}
					<div class="px-4 py-3 flex items-center gap-4">
						<span class="text-sm text-text w-32">{q.description}</span>
						<div class="flex-1">
							{#if hourlyLimit > 0}
								<div class="w-full h-1.5 bg-border rounded-full overflow-hidden">
									<div class="h-full rounded-full transition-all duration-500 {pct > 80 ? 'bg-amber-400' : 'bg-emerald-400/60'}"
										style="width: {Math.min(pct, 100)}%"></div>
								</div>
							{/if}
						</div>
						<span class="text-xs font-mono text-text-muted w-24 text-right">
							{q.calls_this_hour}/{hourlyLimit > 0 ? hourlyLimit.toLocaleString() : '--'}
						</span>
						<span class="text-[10px] text-text-muted w-16 text-right">{q.calls_today} today</span>
					</div>
				{/each}
			</div>
		{/if}
	{/if}
</div>
