<script lang="ts">
	import { isTauri } from '$lib/tauri/mock';
	import {
		getCrossSignals, getConvergenceAlerts, getEntityPrices, getPortfolio,
		executeTrade, closePosition, getFinancialEvents, getSignalEvidence, getSourceHealth,
		getFinancialQuotas, refreshPrices, getPortfolioAnalytics, getTradeJournal,
		runBacktest, startPriceStream, stopPriceStream
	} from '$lib/tauri/commands';
	import type {
		CrossSignal, EntityPrice, Portfolio, FinancialEvent,
		SignalEvidence, SourceHealth, FinancialApiQuota, PortfolioAnalytics,
		TradeJournal, BacktestConfig, BacktestResult, PriceUpdate
	} from '$lib/tauri/types';
	import FreshnessPill from '$lib/components/shared/FreshnessPill.svelte';

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
	let analytics = $state<PortfolioAnalytics | null>(null);
	let showAnalytics = $state(false);
	let expandedTradeId = $state<number | null>(null);
	let tradeJournals = $state<Record<number, TradeJournal>>({});
	let showBacktest = $state(false);
	let backtestRunning = $state(false);
	let backtestResult = $state<BacktestResult | null>(null);
	let backtestError = $state<string | null>(null);
	let expandedBtTrade = $state<number | null>(null);
	let streaming = $state(false);
	let streamError = $state<string | null>(null);
	let btMinScore = $state(0.4);
	let btStopLoss = $state(-10);
	let btTakeProfit = $state(15);
	let btMaxHold = $state(90);
	let btMaxPositions = $state(10);
	let btPositionSize = $state(5);

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadData();
	});

	// Live price stream: subscribe to 'price-updates' emitted from live_prices.rs
	// and merge into portfolio.positions[] so the P&L strip ticks without a reload.
	// Dynamic import per feedback_tauri_imports — never static-import @tauri-apps/api
	// at module scope in Svelte (kills the page).
	$effect(() => {
		if (!isTauri()) return;
		let unlisten: (() => void) | null = null;
		let cancelled = false;
		(async () => {
			try {
				const { listen } = await import('@tauri-apps/api/event');
				if (cancelled) return;
				const un = await listen<PriceUpdate[]>('price-updates', (ev) => {
					const updates = ev.payload;
					if (!updates || updates.length === 0 || !portfolio) return;
					const priceBySymbol = new Map<string, number>();
					for (const u of updates) priceBySymbol.set(u.symbol, u.price);
					// Rebuild positions with updated marks. Leaves untouched any
					// symbol not in this batch (partial updates are normal).
					portfolio = {
						...portfolio,
						positions: portfolio.positions.map(pos => {
							const live = priceBySymbol.get(pos.symbol);
							if (live == null || live <= 0) return pos;
							const market_value = live * pos.qty;
							const cost_basis = pos.avg_entry_price * pos.qty;
							const unrealized_pl = market_value - cost_basis;
							const unrealized_pl_pct = cost_basis > 0 ? unrealized_pl / cost_basis : 0;
							return { ...pos, current_price: live, market_value, unrealized_pl, unrealized_pl_pct };
						}),
					};
				});
				if (cancelled) { un(); return; }
				unlisten = un;
			} catch {
				// If event API isn't available for any reason, silently skip —
				// stale positions beat a crashed page.
			}
		})();
		return () => { cancelled = true; if (unlisten) unlisten(); };
	});

	async function loadData() {
		error = null;
		isLoading = true;
		try {
			if (!isTauri()) { isLoading = false; return; }
			const [s, c, ev, e, p] = await Promise.all([
				getCrossSignals(30).catch(() => []),
				getConvergenceAlerts(40).catch(() => []),
				getSignalEvidence(40).catch(() => []),
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
			getPortfolioAnalytics().then(a => { analytics = a; }).catch(() => {});
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

	let closingId = $state<number | null>(null);

	async function handleClose(tradeId: number, symbol: string) {
		if (!confirm(`Close your ${symbol} position? This places a market sell on Alpaca.`)) return;
		closingId = tradeId;
		tradeStatus = `Closing ${symbol}...`;
		try {
			const result = await closePosition(tradeId);
			const pnl = result.pnl_pct ?? 0;
			tradeStatus = `Closed ${result.ticker} @ $${(result.exit_price ?? 0).toFixed(2)} (${pnl >= 0 ? '+' : ''}${pnl.toFixed(1)}%)`;
			getPortfolio().then(pf => { portfolio = pf; }).catch(() => {});
		} catch (e: any) {
			tradeStatus = `${e?.message ?? e}`;
		} finally {
			closingId = null;
		}
	}

	async function toggleStream() {
		streamError = null;
		if (streaming) {
			await stopPriceStream().catch(() => {});
			streaming = false;
			return;
		}
		try {
			await startPriceStream();
			streaming = true;
		} catch (e: any) {
			streamError = String(e?.message ?? e);
		}
	}

	async function handleBacktest() {
		backtestRunning = true;
		backtestError = null;
		try {
			const end = new Date().toISOString().slice(0, 10);
			const start = new Date(Date.now() - 30 * 86400000).toISOString().slice(0, 10);
			const config: BacktestConfig = {
				start_date: start, end_date: end, min_score: btMinScore,
				stop_loss_pct: btStopLoss, take_profit_pct: btTakeProfit,
				max_hold_days: btMaxHold, max_positions: btMaxPositions,
				position_size_pct: btPositionSize,
			};
			backtestResult = await runBacktest(config);
		} catch (e: any) {
			backtestError = String(e?.message ?? e);
		} finally {
			backtestRunning = false;
		}
	}

	async function toggleTradeJournal(tradeId: number) {
		if (expandedTradeId === tradeId) { expandedTradeId = null; return; }
		expandedTradeId = tradeId;
		if (!tradeJournals[tradeId]) {
			try {
				const j = await getTradeJournal(tradeId);
				tradeJournals = { ...tradeJournals, [tradeId]: j };
			} catch { /* will show loading state */ }
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

	/**
	 * Determine if a Buy button should be disabled due to stale data.
	 * Signal >24h old OR price >48h old OR price missing → stale.
	 * Returns { stale, reason } for UI.
	 */
	function signalStale(ev: { computed_at: string | null; price_date: string | null; price: number | null }): { stale: boolean; reason: string } {
		if (ev.price == null) return { stale: true, reason: 'no price data' };
		const now = Date.now();
		if (ev.computed_at) {
			const age_h = (now - Date.parse(ev.computed_at + 'Z')) / 3_600_000;
			if (age_h > 24) return { stale: true, reason: `signal ${Math.floor(age_h)}h old` };
		}
		if (ev.price_date) {
			const age_h = (now - Date.parse(ev.price_date + 'T00:00:00Z')) / 3_600_000;
			if (age_h > 48) return { stale: true, reason: `price ${Math.floor(age_h / 24)}d old` };
		} else {
			return { stale: true, reason: 'price missing' };
		}
		return { stale: false, reason: '' };
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
		if (status === 'stale') return 'bg-amber-400'; // firing but produced no data in 7d
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

	// --- Convergence Watchlist (intelligence tool, NOT auto-trading) ---
	// The auto-trading strategy backtested as no-edge (NO-GO, 2026-06-05), so the
	// convergence engine is surfaced as a RESEARCH watchlist: "these companies have
	// multiple independent signals stacking — go look", not "buy this".
	const SIGNAL_LABELS: Record<string, { key: keyof CrossSignal; label: string; color: string }[]> = {
		all: [
			{ key: 'insider_signal', label: 'Insider', color: 'text-blue-300 bg-blue-500/10' },
			{ key: 'news_momentum', label: 'News', color: 'text-amber-300 bg-amber-500/10' },
			{ key: 'government_signal', label: 'Gov', color: 'text-emerald-300 bg-emerald-500/10' },
			{ key: 'political_signal', label: 'Lobby', color: 'text-purple-300 bg-purple-500/10' },
			{ key: 'institutional_flow', label: 'Institutional', color: 'text-cyan-300 bg-cyan-500/10' },
			{ key: 'patent_signal', label: 'Patent', color: 'text-pink-300 bg-pink-500/10' },
			{ key: 'search_trend', label: 'Search', color: 'text-indigo-300 bg-indigo-500/10' },
			{ key: 'supply_chain', label: 'Supply', color: 'text-orange-300 bg-orange-500/10' },
		],
	};

	// Which signals are firing (>0.3 normalized) for a convergence row.
	function firingSignals(cs: CrossSignal) {
		return SIGNAL_LABELS.all.filter(s => (cs[s.key] as number) > 0.3);
	}

	function isFreshToday(computedAt: string | null): boolean {
		if (!computedAt) return false;
		const today = new Date().toISOString().slice(0, 10);
		return computedAt.slice(0, 10) === today;
	}

	// Clean the "(CIK 0001234567)" suffix off entity names for display.
	function cleanName(name: string): string {
		return name.replace(/\s*\(CIK\s*\d+\)\s*/i, '').trim();
	}

	// Default to ticker-mapped companies only: having a ticker is a strong proxy
	// for "real researchable public company" — it filters out the entity-extraction
	// noise (CIA, MIT, lobbying disclosures, contract awards) that otherwise
	// pollutes the list. Toggle off to see everything including unmapped entities.
	let publicOnly = $state(true);

	// Watchlist = converging companies, strongest first. The backend returns one
	// row per entity, but distinct entities can share a ticker (Google + Alphabet
	// → GOOGL), so dedup by ticker (keep highest score); unmapped rows dedup by
	// entity_id.
	const watchlist = $derived.by(() => {
		const filtered = convergence.filter(cs => !publicOnly || !!cs.ticker);
		const best = new Map<string, CrossSignal>();
		for (const cs of filtered) {
			const key = cs.ticker ?? `e:${cs.entity_id}`;
			const prev = best.get(key);
			if (!prev || cs.compound_score > prev.compound_score) best.set(key, cs);
		}
		return [...best.values()].sort((a, b) => b.compound_score - a.compound_score);
	});
	const freshCount = $derived(watchlist.filter(w => isFreshToday(w.computed_at)).length);
	const hiddenCount = $derived(convergence.filter(cs => !cs.ticker).length);

	// Match a watchlist company to its evidence (reasons + driving headlines) by
	// name — get_signal_evidence is already fetched, no extra call. Lets each card
	// expand to show WHY it's converging.
	const evidenceByName = $derived.by(() => {
		const m = new Map<string, typeof evidence[number]>();
		for (const e of evidence) m.set(e.entity_name, e);
		return m;
	});
	let expandedWatch = $state<number | null>(null);

	// Keyboard shortcuts
	function handleKeydown(e: KeyboardEvent) {
		if (e.target instanceof HTMLInputElement) return;
		if (e.key === '1') activeTab = 'overview';
		if (e.key === '2') activeTab = 'market';
		if (e.key === '3') activeTab = 'portfolio';
		if (e.key === '4') activeTab = 'sources';
		if (e.key === 'r') loadData();
		if (e.key === 'a') { activeTab = 'portfolio'; showAnalytics = !showAnalytics; }
		if (e.key === 'b') { activeTab = 'portfolio'; showBacktest = !showBacktest; }
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
			{#if signals.length > 0 && signals[0].computed_at}
				<div class="mt-1">
					<FreshnessPill timestamp={signals[0].computed_at} label="Signals" />
				</div>
			{/if}
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
		<div class="bg-bg-card border border-border rounded-xl p-3" title="Sum of unrealized P&L across open Alpaca positions. Does not include realized P&L from closed trades.">
			<div class="text-[10px] text-text-muted uppercase tracking-wider">Unrealized P&L</div>
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

		<!-- ===== Convergence Watchlist (research intelligence, not auto-trading) ===== -->
		{#if convergence.length > 0}
			<div class="mb-6">
				<div class="flex items-center justify-between mb-3">
					<h2 class="text-xs font-semibold text-text uppercase tracking-wider">Convergence Watchlist</h2>
					<div class="flex items-center gap-3">
						<span class="text-[10px] text-text-muted">
							{watchlist.length} converging · {freshCount} new today
						</span>
						<button
							onclick={() => publicOnly = !publicOnly}
							class="text-[10px] px-2 py-1 rounded-md border transition-colors {publicOnly ? 'border-border text-text-muted hover:text-text-secondary' : 'border-blue-500/30 text-blue-300 bg-blue-500/10'}"
							title={publicOnly ? `Showing public companies only. ${hiddenCount} unmapped entities hidden (agencies, private firms, noise).` : 'Showing all converging entities, including unmapped'}>
							{publicOnly ? `Public only` : `All entities`}
						</button>
					</div>
				</div>
				<p class="text-xs text-text-muted mb-3 leading-relaxed">
					Companies where multiple independent signals are stacking. These are
					<span class="text-text-secondary">research candidates</span> — surfaced for you to investigate, not trade automatically.
				</p>
				{#if watchlist.length === 0}
					<p class="text-xs text-text-muted italic">No public companies converging right now. Toggle "All entities" to see unmapped signals.</p>
				{/if}
				<div class="space-y-2">
					{#each watchlist as cs, idx}
						{@const firing = firingSignals(cs)}
						{@const fresh = isFreshToday(cs.computed_at)}
						{@const ev = evidenceByName.get(cs.entity_name)}
						{@const isOpen = expandedWatch === idx}
						{@const stories = ev?.source_stories ?? []}
						<div class="bg-bg-card border border-border rounded-xl overflow-hidden">
							<button class="w-full text-left px-5 py-4 hover:bg-white/[0.02] transition-colors"
								onclick={() => expandedWatch = isOpen ? null : idx}>
								<div class="flex items-start justify-between gap-3">
									<div class="min-w-0">
										<div class="flex items-center gap-2 flex-wrap">
											<span class="font-semibold text-text truncate">{cleanName(cs.entity_name)}</span>
											{#if cs.ticker}
												<span class="text-[10px] font-mono text-text-muted bg-bg px-1.5 py-0.5 rounded">{cs.ticker}</span>
											{/if}
											{#if fresh}
												<span class="text-[9px] font-semibold uppercase tracking-wide text-emerald-400 bg-emerald-500/10 px-1.5 py-0.5 rounded">New</span>
											{/if}
										</div>
										<!-- Firing-signal badges: WHY this is converging -->
										<div class="flex items-center gap-1.5 flex-wrap mt-2">
											{#each firing as sig}
												<span class="text-[10px] font-medium px-1.5 py-0.5 rounded {sig.color}">{sig.label}</span>
											{/each}
											<span class="text-[10px] text-text-muted ml-1">{cs.source_diversity} source{cs.source_diversity === 1 ? '' : 's'}</span>
											{#if stories.length > 0}
												<span class="text-[10px] text-text-muted">· {isOpen ? '▾' : '▸'} {stories.length} headline{stories.length === 1 ? '' : 's'}</span>
											{/if}
										</div>
									</div>
									<div class="text-right shrink-0">
										<div class="text-base font-mono font-bold text-text">{(cs.compound_score * 100).toFixed(0)}%</div>
										<div class="text-[10px] text-text-muted">convergence</div>
									</div>
								</div>
							</button>
							{#if isOpen}
								<div class="px-5 pb-4 pt-1 border-t border-border/50">
									{#if ev?.reasons?.length}
										<ul class="mb-3 space-y-1">
											{#each ev.reasons as reason}
												<li class="text-xs text-text-secondary flex gap-1.5"><span class="text-text-muted">·</span>{reason}</li>
											{/each}
										</ul>
									{/if}
									{#if stories.length > 0}
										<div class="text-[10px] font-semibold text-text-muted uppercase tracking-wider mb-1.5">Recent headlines</div>
										<div class="space-y-1.5">
											{#each stories.slice(0, 6) as story}
												<div class="text-xs">
													<span class="text-text-secondary">{story.headline}</span>
													<span class="text-[10px] text-text-muted ml-1">— {story.source_name}</span>
												</div>
											{/each}
										</div>
									{:else}
										<p class="text-xs text-text-muted italic">No headline evidence available for this company yet.</p>
									{/if}
								</div>
							{/if}
						</div>
					{/each}
				</div>
			</div>
		{/if}

		<!-- Manual trade shortcut — NOT an instruction. The auto-trading strategy
		     backtested as no-edge; buying here is YOUR discretionary call. -->
		{#if actionable.length > 0}
			<h2 class="text-xs font-semibold text-text-secondary uppercase tracking-wider mb-1">Manual Trade · Discretionary</h2>
			<p class="text-[11px] text-text-muted mb-3">Highest-scoring signals if you want to trade one by hand. Not a recommendation — the auto-strategy showed no edge.</p>
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
								{@const stale = signalStale(ev)}
								<button
									class="px-4 py-1.5 rounded-lg text-xs font-semibold transition-colors
										{stale.stale
											? 'bg-amber-500/10 text-amber-400 border border-amber-500/25 cursor-not-allowed'
											: 'bg-emerald-500/15 text-emerald-400 border border-emerald-500/25 hover:bg-emerald-500/25'}"
									onclick={() => !stale.stale && handleTrade(ev.ticker!, ev.compound_score)}
									disabled={stale.stale}
									title={stale.stale ? `Stale — ${stale.reason}` : `Buy ${ev.ticker}`}
								>
									{stale.stale ? `Stale · ${stale.reason}` : `Buy ${ev.ticker}`}
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
										{@const stale2 = signalStale(ev)}
										<button
											class="px-4 py-1.5 rounded-lg text-xs font-medium transition-colors
												{stale2.stale
													? 'bg-amber-500/10 text-amber-400 border border-amber-500/25 cursor-not-allowed'
													: 'bg-ai/10 text-ai border border-ai/25 hover:bg-ai/20'}"
											onclick={() => !stale2.stale && handleTrade(ev.ticker!, ev.compound_score)}
											disabled={stale2.stale}
											title={stale2.stale ? `Stale — ${stale2.reason}` : `Buy ${ev.ticker}`}
										>{stale2.stale ? `Stale · ${stale2.reason}` : `Buy ${ev.ticker}`}</button>
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
			<div class="bg-bg-card border {totalPnl >= 0 ? 'border-emerald-500/20' : 'border-rose-500/20'} rounded-xl p-6 mb-5 text-center relative">
				<div class="text-[10px] text-text-muted uppercase tracking-wider mb-1">Total Unrealized P&L</div>
				<div class="text-3xl font-mono font-bold {totalPnl >= 0 ? 'text-emerald-400' : 'text-rose-400'}">
					{fmtPnl(totalPnl)}
				</div>
				<div class="text-sm font-mono {totalPnlPct >= 0 ? 'text-emerald-400/70' : 'text-rose-400/70'}">
					{totalPnlPct >= 0 ? '+' : ''}{totalPnlPct.toFixed(2)}%
				</div>
				<button onclick={toggleStream}
					title={streaming ? 'Live Finnhub stream active — positions tick as trades print' : 'Start a Finnhub WebSocket stream to see positions tick live'}
					class="absolute top-3 right-3 flex items-center gap-1.5 px-2 py-1 rounded-md text-[10px] transition-colors {streaming ? 'bg-emerald-500/15 text-emerald-400 hover:bg-emerald-500/25' : 'bg-zinc-500/10 text-text-muted hover:bg-zinc-500/20'}">
					<span class="w-1.5 h-1.5 rounded-full {streaming ? 'bg-emerald-400 animate-pulse' : 'bg-zinc-500'}"></span>
					{streaming ? 'LIVE' : 'Stream'}
				</button>
				{#if streamError}<p class="text-[10px] text-rose-400 mt-1">{streamError}</p>{/if}
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

			<!-- Analytics Toggle -->
			<button onclick={() => showAnalytics = !showAnalytics}
				class="w-full text-left mb-5 flex items-center justify-between px-4 py-2.5 bg-bg-card border border-border rounded-xl hover:border-ai/30 transition-colors">
				<span class="text-xs font-semibold text-text-muted uppercase tracking-wider">Analytics {analytics?.total_trades ? `(${analytics.total_trades} trades)` : ''}</span>
				<span class="text-xs text-text-muted">{showAnalytics ? '▾' : '▸'} <kbd class="text-[10px] px-1 py-0.5 bg-bg rounded border border-border ml-1">a</kbd></span>
			</button>

			{#if showAnalytics && analytics && analytics.total_trades > 0}
				<div class="grid grid-cols-3 gap-3 mb-4">
					<div class="bg-bg-card border border-border rounded-xl p-4 text-center">
						<div class="text-[10px] text-text-muted uppercase tracking-wider mb-1">Win Rate</div>
						<div class="text-2xl font-mono font-bold {analytics.win_rate >= 50 ? 'text-emerald-400' : 'text-rose-400'}">{analytics.win_rate.toFixed(0)}%</div>
					</div>
					<div class="bg-bg-card border border-border rounded-xl p-4 text-center">
						<div class="text-[10px] text-text-muted uppercase tracking-wider mb-1">Profit Factor</div>
						<div class="text-2xl font-mono font-bold {analytics.profit_factor >= 1 ? 'text-emerald-400' : 'text-rose-400'}">{analytics.profit_factor === Infinity ? '∞' : analytics.profit_factor.toFixed(2)}</div>
					</div>
					<div class="bg-bg-card border border-border rounded-xl p-4 text-center">
						<div class="text-[10px] text-text-muted uppercase tracking-wider mb-1">Sharpe Ratio</div>
						<div class="text-2xl font-mono font-bold {analytics.sharpe_ratio >= 1 ? 'text-emerald-400' : analytics.sharpe_ratio >= 0 ? 'text-amber-400' : 'text-rose-400'}">{analytics.sharpe_ratio.toFixed(2)}</div>
					</div>
				</div>
				<div class="grid grid-cols-2 gap-3 mb-4">
					<div class="bg-bg-card border border-border rounded-xl p-4">
						<div class="text-[10px] text-text-muted uppercase tracking-wider mb-2">Returns</div>
						<div class="space-y-1.5">
							<div class="flex justify-between text-xs" title="Realized P&L from closed trades only. Open positions' unrealized P&L shown separately in the hero."><span class="text-text-muted">Realized P&L (closed)</span><span class="font-mono font-semibold {analytics.total_return_dollars >= 0 ? 'text-emerald-400' : 'text-rose-400'}">{analytics.total_return_dollars >= 0 ? '+' : ''}{fmtPrice(analytics.total_return_dollars)}</span></div>
							<div class="flex justify-between text-xs"><span class="text-text-muted">Avg Win</span><span class="font-mono text-emerald-400">+{analytics.avg_win_pct.toFixed(1)}%</span></div>
							<div class="flex justify-between text-xs"><span class="text-text-muted">Avg Loss</span><span class="font-mono text-rose-400">{analytics.avg_loss_pct.toFixed(1)}%</span></div>
						</div>
					</div>
					<div class="bg-bg-card border border-border rounded-xl p-4">
						<div class="text-[10px] text-text-muted uppercase tracking-wider mb-2">Risk</div>
						<div class="space-y-1.5">
							<div class="flex justify-between text-xs"><span class="text-text-muted">Max Drawdown</span><span class="font-mono text-rose-400">-{analytics.max_drawdown_pct.toFixed(1)}%</span></div>
							<div class="flex justify-between text-xs"><span class="text-text-muted">Avg Hold</span><span class="font-mono text-text">{analytics.avg_holding_days.toFixed(0)} days</span></div>
							{#if analytics.best_trade}<div class="flex justify-between text-xs"><span class="text-text-muted">Best</span><span class="font-mono text-emerald-400">{analytics.best_trade.ticker} +{analytics.best_trade.pnl_pct.toFixed(1)}%</span></div>{/if}
							{#if analytics.worst_trade}<div class="flex justify-between text-xs"><span class="text-text-muted">Worst</span><span class="font-mono text-rose-400">{analytics.worst_trade.ticker} {analytics.worst_trade.pnl_pct.toFixed(1)}%</span></div>{/if}
						</div>
					</div>
				</div>
				{#if analytics.signal_attribution.some(s => s.sample_count > 0)}
					<div class="bg-bg-card border border-border rounded-xl p-4 mb-5">
						<div class="text-[10px] text-text-muted uppercase tracking-wider mb-3">Signal Attribution</div>
						<div class="space-y-2">
							{#each analytics.signal_attribution.filter(s => s.sample_count > 0) as sig}
								<div class="flex items-center gap-3">
									<span class="text-xs text-text w-24 capitalize">{sig.dimension}</span>
									<div class="flex-1 h-1.5 bg-border rounded-full overflow-hidden"><div class="h-full rounded-full {sig.predictive_edge > 0 ? 'bg-emerald-400/60' : 'bg-rose-400/60'}" style="width: {Math.min(Math.abs(sig.predictive_edge) * 250, 50) + 50}%"></div></div>
									<span class="text-[10px] font-mono w-12 text-right {sig.predictive_edge > 0 ? 'text-emerald-400' : sig.predictive_edge < 0 ? 'text-rose-400' : 'text-text-muted'}">{sig.predictive_edge >= 0 ? '+' : ''}{sig.predictive_edge.toFixed(2)}</span>
									<span class="text-[10px] text-text-muted w-8 text-right">n={sig.sample_count}</span>
								</div>
							{/each}
						</div>
					</div>
				{/if}
			{:else if showAnalytics}
				<div class="bg-bg-card border border-border rounded-xl p-6 mb-5 text-center">
					<p class="text-sm text-text-muted">No closed trades yet — analytics appear after your first trade closes.</p>
				</div>
			{/if}

			<!-- Backtester Toggle -->
			<button onclick={() => showBacktest = !showBacktest}
				class="w-full text-left mb-5 flex items-center justify-between px-4 py-2.5 bg-bg-card border border-border rounded-xl hover:border-ai/30 transition-colors">
				<span class="text-xs font-semibold text-text-muted uppercase tracking-wider">Backtester</span>
				<span class="text-xs text-text-muted">{showBacktest ? '▾' : '▸'} <kbd class="text-[10px] px-1 py-0.5 bg-bg rounded border border-border ml-1">b</kbd></span>
			</button>

			{#if showBacktest}
				<div class="bg-bg-card border border-border rounded-xl p-4 mb-5">
					<div class="grid grid-cols-3 gap-3 mb-3">
						<div><label for="bt-min-score" class="text-[10px] text-text-muted block mb-1">Min Score</label><input id="bt-min-score" type="number" bind:value={btMinScore} min="0.1" max="1.0" step="0.05" class="w-full bg-bg border border-border rounded-lg px-2 py-1.5 text-xs text-text font-mono focus:outline-none focus:border-ai" /></div>
						<div><label for="bt-stop-loss" class="text-[10px] text-text-muted block mb-1">Stop Loss %</label><input id="bt-stop-loss" type="number" bind:value={btStopLoss} max="0" step="1" class="w-full bg-bg border border-border rounded-lg px-2 py-1.5 text-xs text-text font-mono focus:outline-none focus:border-ai" /></div>
						<div><label for="bt-take-profit" class="text-[10px] text-text-muted block mb-1">Take Profit %</label><input id="bt-take-profit" type="number" bind:value={btTakeProfit} min="1" step="1" class="w-full bg-bg border border-border rounded-lg px-2 py-1.5 text-xs text-text font-mono focus:outline-none focus:border-ai" /></div>
						<div><label for="bt-max-hold" class="text-[10px] text-text-muted block mb-1">Max Hold (days)</label><input id="bt-max-hold" type="number" bind:value={btMaxHold} min="1" max="365" step="1" class="w-full bg-bg border border-border rounded-lg px-2 py-1.5 text-xs text-text font-mono focus:outline-none focus:border-ai" /></div>
						<div><label for="bt-max-pos" class="text-[10px] text-text-muted block mb-1">Max Positions</label><input id="bt-max-pos" type="number" bind:value={btMaxPositions} min="1" max="50" step="1" class="w-full bg-bg border border-border rounded-lg px-2 py-1.5 text-xs text-text font-mono focus:outline-none focus:border-ai" /></div>
						<div><label for="bt-pos-size" class="text-[10px] text-text-muted block mb-1">Position Size %</label><input id="bt-pos-size" type="number" bind:value={btPositionSize} min="1" max="25" step="1" class="w-full bg-bg border border-border rounded-lg px-2 py-1.5 text-xs text-text font-mono focus:outline-none focus:border-ai" /></div>
					</div>
					<button onclick={handleBacktest} disabled={backtestRunning}
						class="px-4 py-2 bg-ai/10 text-ai border border-ai/25 rounded-lg text-xs font-medium hover:bg-ai/20 transition-colors disabled:opacity-50">{backtestRunning ? 'Running...' : 'Run Backtest (30 days)'}</button>
				</div>
				{#if backtestError}<div class="bg-rose-500/10 border border-rose-500/20 rounded-xl p-3 mb-5"><p class="text-xs text-rose-400">{backtestError}</p></div>{/if}
				{#if backtestResult}
					<div class="mb-5">
						<div class="grid grid-cols-4 gap-2 mb-3">
							<div class="bg-bg border border-border rounded-lg p-2.5 text-center"><div class="text-[10px] text-text-muted">Hit Rate</div><div class="text-lg font-mono font-bold {backtestResult.hit_rate >= 50 ? 'text-emerald-400' : 'text-rose-400'}">{backtestResult.hit_rate.toFixed(0)}%</div><div class="text-[10px] text-text-muted">{backtestResult.trades_won}W / {backtestResult.trades_lost}L</div></div>
							<div class="bg-bg border border-border rounded-lg p-2.5 text-center"><div class="text-[10px] text-text-muted">Total Return</div><div class="text-lg font-mono font-bold {backtestResult.total_return_pct >= 0 ? 'text-emerald-400' : 'text-rose-400'}">{backtestResult.total_return_pct >= 0 ? '+' : ''}{backtestResult.total_return_pct.toFixed(1)}%</div></div>
							<div class="bg-bg border border-border rounded-lg p-2.5 text-center"><div class="text-[10px] text-text-muted">Sharpe</div><div class="text-lg font-mono font-bold {backtestResult.sharpe_ratio >= 1 ? 'text-emerald-400' : 'text-amber-400'}">{backtestResult.sharpe_ratio.toFixed(2)}</div></div>
							<div class="bg-bg border border-border rounded-lg p-2.5 text-center"><div class="text-[10px] text-text-muted">Max DD</div><div class="text-lg font-mono font-bold text-rose-400">-{backtestResult.max_drawdown_pct.toFixed(1)}%</div></div>
						</div>
						{#if backtestResult.equity_curve && backtestResult.equity_curve.length >= 2}
							{@const curve = backtestResult.equity_curve}
							{@const minV = Math.min(...curve.map(p => p.value))}
							{@const maxV = Math.max(...curve.map(p => p.value))}
							{@const range = (maxV - minV) || 1}
							{@const start = backtestResult.starting_equity}
							<div class="bg-bg border border-border rounded-xl p-3 mb-3">
								<div class="flex items-center justify-between mb-2">
									<span class="text-[10px] text-text-muted uppercase tracking-wider">Equity Curve</span>
									<span class="text-[10px] font-mono text-text-muted">${Math.round(curve[0].value).toLocaleString()} → ${Math.round(curve[curve.length - 1].value).toLocaleString()}</span>
								</div>
								<svg viewBox="0 0 200 40" preserveAspectRatio="none" class="w-full h-10">
									<polyline
										fill="none"
										stroke-width="1.2"
										class="{backtestResult.total_return_pct >= 0 ? 'stroke-emerald-400' : 'stroke-rose-400'}"
										points={curve.map((p, i) => `${(i / (curve.length - 1)) * 200},${40 - ((p.value - minV) / range) * 38 - 1}`).join(' ')}
									/>
									<line x1="0" x2="200" y1={40 - ((start - minV) / range) * 38 - 1} y2={40 - ((start - minV) / range) * 38 - 1} stroke="currentColor" stroke-dasharray="2,2" class="text-text-muted/40" stroke-width="0.5" />
								</svg>
							</div>
						{/if}
						{#if backtestResult.monthly_returns && backtestResult.monthly_returns.length > 0}
							{@const months = backtestResult.monthly_returns}
							{@const maxAbs = Math.max(...months.map(m => Math.abs(m.pnl_pct)), 0.01)}
							<div class="bg-bg border border-border rounded-xl p-3 mb-3">
								<div class="text-[10px] text-text-muted uppercase tracking-wider mb-2">Monthly Returns</div>
								<div class="flex items-end gap-1 h-14">
									{#each months as m}
										{@const heightPct = (Math.abs(m.pnl_pct) / maxAbs) * 100}
										<div class="flex-1 flex flex-col items-center gap-0.5" title="{m.month}: {m.pnl_pct >= 0 ? '+' : ''}{m.pnl_pct.toFixed(1)}% · {m.trade_count} trade{m.trade_count === 1 ? '' : 's'} closed">
											<div class="w-full flex items-end justify-center" style="height: {heightPct}%">
												<div class="w-full rounded-t {m.pnl_pct >= 0 ? 'bg-emerald-400/70' : 'bg-rose-400/70'}" style="height: 100%"></div>
											</div>
											<span class="text-[8px] font-mono text-text-muted">{m.month.slice(5)}</span>
										</div>
									{/each}
								</div>
							</div>
						{/if}
						{#if backtestResult.trades.length > 0}
							<div class="bg-bg-card border border-border rounded-xl divide-y divide-border/50">
								{#each backtestResult.trades as bt, i}
									{@const isWin = bt.pnl_pct > 0}
									<button class="w-full text-left px-4 py-2.5 hover:bg-white/[0.02] transition-colors" onclick={() => expandedBtTrade = expandedBtTrade === i ? null : i}>
										<div class="flex items-center justify-between">
											<div class="flex items-center gap-2">
												<span class="w-1.5 h-1.5 rounded-full {isWin ? 'bg-emerald-400' : 'bg-rose-400'}"></span>
												<span class="text-xs font-mono font-semibold text-text">{bt.ticker}</span>
												<span class="text-[10px] px-1.5 py-0.5 rounded font-medium {bt.exit_reason === 'take_profit' ? 'bg-emerald-500/15 text-emerald-400' : bt.exit_reason === 'stop_loss' ? 'bg-rose-500/15 text-rose-400' : 'bg-zinc-500/15 text-zinc-400'}">{bt.exit_reason.replace('_', ' ')}</span>
											</div>
											<span class="text-xs font-mono font-bold {isWin ? 'text-emerald-400' : 'text-rose-400'}">{isWin ? '+' : ''}{bt.pnl_pct.toFixed(1)}%</span>
										</div>
										{#if expandedBtTrade === i}
											<div class="mt-1 text-[11px] text-text-muted flex gap-4">
												<span>Entry: {bt.entry_date} @ ${bt.entry_price.toFixed(2)}</span>
												<span>Exit: {bt.exit_date} @ ${bt.exit_price.toFixed(2)}</span>
												<span>{bt.holding_days}d</span>
											</div>
										{/if}
									</button>
								{/each}
							</div>
						{:else}
							<p class="text-xs text-text-muted text-center py-3">No signals matched criteria.</p>
						{/if}
					</div>
				{/if}
			{/if}

			<!-- Open Positions -->
			{#if portfolio.positions.length > 0}
				<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Open Positions</h2>
				<div class="space-y-2 mb-6">
					{#each portfolio.positions as pos}
						{@const pnlPct = pos.unrealized_pl_pct * 100}
						{@const isWinning = pos.unrealized_pl >= 0}
						{@const dbTrade = portfolio.open_trades.find(t => t.ticker === pos.symbol)}
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
								<span>Cost: {fmtPrice(pos.avg_entry_price * pos.qty)} &rarr; {fmtPrice(pos.market_value)}</span>
							</div>
							<!-- P&L bar -->
							<div class="mt-2 h-1 bg-border rounded-full overflow-hidden">
								<div class="h-full rounded-full {isWinning ? 'bg-emerald-400' : 'bg-rose-400'}" style="width: {Math.min(Math.abs(pnlPct) * 5, 100)}%"></div>
							</div>
							{#if dbTrade}
								<div class="mt-3 flex justify-end">
									<button
										onclick={() => handleClose(dbTrade.id, pos.symbol)}
										disabled={closingId === dbTrade.id}
										class="text-xs font-medium px-3 py-1.5 rounded-lg border border-rose-500/30 text-rose-300 hover:bg-rose-500/10 disabled:opacity-50 transition-colors"
										title="Place a market sell on Alpaca and close this position">
										{closingId === dbTrade.id ? 'Closing…' : 'Close position'}
									</button>
								</div>
							{/if}
						</div>
					{/each}
				</div>
			{/if}

			<!-- Closed Trades (historical ledger — open positions shown above) -->
			{#if portfolio.closed_trades.length > 0}
				<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">Closed Trades</h2>
				<div class="bg-bg-card border border-border rounded-xl divide-y divide-border/50 mb-6">
					{#each portfolio.closed_trades as trade}
						{@const hasPnl = trade.pnl_pct !== null && trade.pnl_pct !== undefined}
						{@const pnl = trade.pnl_pct ?? 0}
						{@const isGreen = pnl >= 0}
						{@const isClosed = trade.status !== 'open'}
						<div>
							<button class="w-full text-left px-4 py-3 {isClosed ? 'hover:bg-white/[0.02]' : ''} transition-colors"
								onclick={() => isClosed && toggleTradeJournal(trade.id)} disabled={!isClosed}>
								<div class="flex items-center justify-between">
									<div class="flex items-center gap-2.5">
										<span class="w-2 h-2 rounded-full {trade.status === 'open' ? (isGreen ? 'bg-emerald-400' : 'bg-rose-400') : 'bg-zinc-500'}"></span>
										<span class="text-sm font-mono font-semibold text-text">{trade.ticker}</span>
										<span class="text-[10px] px-1.5 py-0.5 rounded font-medium {
											trade.status === 'open' ? 'bg-blue-500/15 text-blue-400' :
											trade.status === 'closed' && pnl > 0 ? 'bg-emerald-500/15 text-emerald-400' :
											trade.status === 'stopped_out' ? 'bg-rose-500/15 text-rose-400' :
											'bg-zinc-500/15 text-zinc-400'
										}">{trade.status}</span>
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
										{#if isClosed}<span class="text-[10px] text-text-muted ml-1">{expandedTradeId === trade.id ? '▾' : '▸'}</span>{/if}
									</div>
								</div>
								<div class="flex items-center justify-between mt-1.5 text-[11px] text-text-muted">
									<span>Bought @ ${trade.entry_price.toFixed(2)} &middot; {fmtPrice(trade.position_size)} invested &middot; {trade.entry_date.slice(0, 10)}</span>
									{#if trade.exit_price}
										<span>Sold @ ${trade.exit_price.toFixed(2)} &middot; {trade.exit_date?.slice(0, 10)}</span>
									{/if}
								</div>
							</button>
							{#if expandedTradeId === trade.id && tradeJournals[trade.id]}
								<div class="px-4 pb-3 pt-1">
									<div class="bg-bg border border-border rounded-lg p-3">
										<p class="text-xs text-text-secondary leading-relaxed">{tradeJournals[trade.id].journal}</p>
									</div>
								</div>
							{/if}
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
						<span class="{src.status === 'active' ? 'text-emerald-400' : (src.status === 'migrating' || src.status === 'stale') ? 'text-amber-400' : 'text-zinc-500'}">
							{src.status === 'active' ? 'Active' : src.status === 'migrating' ? 'Migrating' : src.status === 'stale' ? 'No data 7d' : 'Inactive'}
						</span>
					</div>
				</div>
			{/each}
		</div>

		{#if quotas.length > 0}
			<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider mb-3">API Rate Limits</h2>
			<div class="bg-bg-card border border-border rounded-xl divide-y divide-border/50">
				{#each quotas.filter(q => q.limit_per_minute > 0 || q.limit_per_hour > 0 || q.calls_today > 0) as q}
					{@const hourlyLimit = q.limit_per_hour > 0
						? q.limit_per_hour
						: (q.limit_per_minute > 0 ? q.limit_per_minute * 60 : 0)}
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
