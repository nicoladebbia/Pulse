<script lang="ts">
	import { runBacktest, getBacktestRange, getBacktestHistory } from '$lib/tauri/commands';
	import { isTauri, buildMockBacktestRun, mockBacktestRange } from '$lib/tauri/mock';
	import type { BacktestRange, BacktestResult, BacktestRun, BacktestConfig } from '$lib/tauri/types';
	import SettingsCard from '$lib/components/backtest/SettingsCard.svelte';
	import EquityChart from '$lib/components/backtest/EquityChart.svelte';
	import MonthlyBars from '$lib/components/backtest/MonthlyBars.svelte';
	import TradesTable from '$lib/components/backtest/TradesTable.svelte';
	import { liveBotForm, toConfig, validate, exitLabel, type FormState } from '$lib/components/backtest/config';

	const errMsg = (e: unknown) => (typeof e === 'string' ? e : e instanceof Error ? e.message : String(e));
	const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

	// Browser (non-Tauri) fallbacks so the page is demoable.
	async function fetchRange(): Promise<BacktestRange> {
		if (!isTauri()) return mockBacktestRange;
		return getBacktestRange();
	}
	async function fetchHistory(): Promise<BacktestResult[]> {
		if (!isTauri()) {
			const r = buildMockBacktestRun().result;
			return [
				{ ...r, config_summary: 'score>=0.5, fixed -10% / +15%, 5% size, 10 positions', total_return_pct: 4.2, hit_rate: 48.1, trades_taken: 27, sharpe_ratio: 0.61, tickers_signalled: 0 },
				{ ...r, config_summary: 'score>=0.3, ATR trail 3x, 20 positions', total_return_pct: -1.8, hit_rate: 44.0, trades_taken: 61, sharpe_ratio: -0.2, tickers_signalled: 0 }
			];
		}
		return getBacktestHistory();
	}
	async function execute(cfg: BacktestConfig): Promise<BacktestRun> {
		if (!isTauri()) {
			await sleep(600);
			return buildMockBacktestRun();
		}
		return runBacktest(cfg);
	}

	let range = $state<BacktestRange | null>(null);
	let rangeLoading = $state(true);
	let rangeError = $state<string | null>(null);
	let history = $state<BacktestResult[]>([]);
	let historyLoading = $state(true);

	let form = $state<FormState>(liveBotForm('', ''));
	let running = $state(false);
	let error = $state<string | null>(null);
	let run = $state<BacktestRun | null>(null);
	let loadedFromHistory = $state(false);
	let runSeq = 0;

	const res = $derived(run?.result ?? null);
	const spyOn = $derived(!!run && !loadedFromHistory && run.spy_return_pct != null && Number.isFinite(run.spy_return_pct));
	const edge = $derived(res && spyOn ? res.total_return_pct - (run!.spy_return_pct as number) : null);
	const noSignals = $derived(!!res && res.total_signals === 0 && res.trades_taken === 0 && res.open_at_end === 0);

	const reasonCounts = $derived.by(() => {
		const m = new Map<string, number>();
		for (const t of res?.trades ?? []) m.set(t.exit_reason, (m.get(t.exit_reason) ?? 0) + 1);
		return [...m.entries()].sort((a, b) => b[1] - a[1]);
	});

	let booted = false;
	$effect(() => {
		if (booted) return;
		booted = true;
		fetchRange()
			.then((r) => {
				range = r;
				if (r.first_signal_date && r.last_price_date) {
					form.start = r.first_signal_date;
					form.end = r.last_price_date;
				}
			})
			.catch((e) => (rangeError = errMsg(e)))
			.finally(() => (rangeLoading = false));
		fetchHistory()
			.then((h) => (history = h))
			.catch(() => (history = []))
			.finally(() => (historyLoading = false));
	});

	async function start() {
		if (running || Object.keys(validate(form)).length) return;
		running = true;
		error = null;
		const seq = ++runSeq;
		const cfg = toConfig(form);
		try {
			const r = await execute(cfg);
			if (seq !== runSeq) return;
			run = r;
			loadedFromHistory = false;
		} catch (e) {
			if (seq === runSeq) error = errMsg(e);
		} finally {
			if (seq === runSeq) running = false;
		}
		if (seq === runSeq && !error) fetchHistory().then((h) => (history = h)).catch(() => {});
	}

	function loadPast(h: BacktestResult) {
		runSeq++;
		running = false;
		error = null;
		run = { result: h, spy_curve: [], spy_return_pct: null, spy_error: null };
		loadedFromHistory = true;
	}

	function onKey(e: KeyboardEvent) {
		const t = e.target as HTMLElement | null;
		if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable)) return;
		if (e.metaKey || e.ctrlKey || e.altKey) return;
		if (e.key === 'r') {
			e.preventDefault();
			start();
		}
	}

	const pct = (v: number, d = 1) => (Number.isFinite(v) ? `${v >= 0 ? '+' : '−'}${Math.abs(v).toFixed(d)}%` : '—');
	const tone = (v: number) => (v >= 0 ? 'text-emerald-400' : 'text-rose-400');
	const money = (v: number) => v.toLocaleString('en-US', { style: 'currency', currency: 'USD', maximumFractionDigits: 0 });
</script>

<svelte:window onkeydown={onKey} />

<div class="max-w-5xl mx-auto px-6 py-6 space-y-5">
	<header>
		<h1 class="text-xl font-bold text-text">Backtest</h1>
		<p class="text-xs text-text-muted mt-0.5">
			{#if rangeLoading}
				<span class="inline-block h-3 w-64 rounded bg-bg-card-hover animate-pulse align-middle"></span>
			{:else if range?.first_signal_date && range?.last_price_date}
				Signals since <span class="font-mono tabular-nums">{range.first_signal_date}</span>, prices through <span class="font-mono tabular-nums">{range.last_price_date}</span>
			{:else}
				No stored signal or price data yet.
			{/if}
		</p>
		{#if rangeError}<p class="text-xs text-rose-400 mt-1">Couldn’t read the data range: {rangeError}</p>{/if}
	</header>

	<details class="bg-bg-card border border-border rounded-xl px-5 py-3 text-xs text-text-secondary">
		<summary class="cursor-pointer text-text rounded focus-visible:outline-2 focus-visible:outline-ai">How this works</summary>
		<p class="mt-2 leading-relaxed max-w-prose">
			The backtest replays Pulse’s past convergence signals (several independent sources agreeing on a stock) day by day against stored daily prices. It opens positions by the entry and sizing rules you set, closes them by your exit rules, and compounds the equity. It tests the entry signal. It is not a guarantee of live results. Only dates with stored data work, and the range needs at least 30 trading days.
		</p>
	</details>

	<SettingsCard bind:form {range} {running} onrun={start} />

	<!-- Results -->
	<section aria-label="Results" aria-busy={running} class="relative">
		{#if error}
			<div role="alert" class="bg-rose-400/5 border border-rose-400/30 rounded-xl p-5">
				<p class="text-sm font-medium text-rose-400">Backtest failed</p>
				<p class="text-sm text-text-secondary mt-1 break-words">{error}</p>
			</div>
		{:else if !res}
			<div class="bg-bg-card border border-dashed border-border rounded-xl py-14 text-center">
				{#if running}
					<p class="text-sm text-text-secondary"><span class="inline-block w-3 h-3 mr-2 align-[-1px] rounded-full border-2 border-ai border-t-transparent animate-spin"></span>Replaying signals…</p>
				{:else}
					<p class="text-sm text-text">Pick your rules and run</p>
					<p class="text-xs text-text-muted mt-1">Results, the equity curve and every trade will show up here.</p>
				{/if}
			</div>
		{:else if noSignals}
			<div class="bg-bg-card border border-border rounded-xl py-12 text-center">
				<p class="text-sm text-text">No signals matched these settings</p>
				<p class="text-xs text-text-muted mt-1">Try lowering the min score or widening the date range.</p>
			</div>
		{:else}
			<div class="space-y-5 transition-opacity duration-200 {running ? 'opacity-40 pointer-events-none' : ''}">
				{#if loadedFromHistory}
					<p class="text-xs text-text-muted">Saved run: {res.config_summary}. Saved runs don’t include the S&amp;P 500 comparison.</p>
				{:else}
					<p class="text-xs text-text-muted">{res.config_summary}</p>
				{/if}

				<!-- Stat tiles: total return leads -->
				<div class="grid gap-3 grid-cols-2 lg:grid-cols-[1.5fr_1fr_1fr_1fr]">
					<div class="col-span-2 lg:col-span-1 lg:row-span-2 bg-bg-card border border-border rounded-xl p-5 flex flex-col justify-between">
						<div>
							<p class="text-xs text-text-secondary">Total return</p>
							<p class="text-4xl font-semibold font-mono tabular-nums tracking-tight mt-1 {tone(res.total_return_pct)}">{pct(res.total_return_pct)}</p>
							<p class="text-xs font-mono tabular-nums text-text-muted mt-1">{money(res.starting_equity)} to {money(res.ending_equity)}</p>
						</div>
						<div class="mt-4 text-xs">
							{#if spyOn}
								<p class="text-text-secondary">S&amp;P 500 <span class="font-mono tabular-nums {tone(run!.spy_return_pct as number)}">{pct(run!.spy_return_pct as number)}</span></p>
								<p class="mt-0.5 font-mono tabular-nums {tone(edge as number)}">{(edge as number) >= 0 ? '+' : '−'}{Math.abs(edge as number).toFixed(1)} pts {(edge as number) >= 0 ? 'ahead of' : 'behind'} the market</p>
							{:else if run?.spy_error && !loadedFromHistory}
								<p class="text-text-muted">S&amp;P 500 comparison unavailable: {run.spy_error}</p>
							{/if}
						</div>
					</div>
					<div class="bg-bg-card border border-border rounded-xl p-4">
						<p class="text-xs text-text-secondary">Hit rate</p>
						<p class="text-xl font-semibold font-mono tabular-nums mt-1 text-text">{Number.isFinite(res.hit_rate) ? res.hit_rate.toFixed(1) : '—'}%</p>
						<p class="text-xs font-mono tabular-nums text-text-muted mt-0.5">{res.trades_won}W / {res.trades_lost}L</p>
					</div>
					<div class="bg-bg-card border border-border rounded-xl p-4">
						<p class="text-xs text-text-secondary">Max drawdown</p>
						<p class="text-xl font-semibold font-mono tabular-nums mt-1 text-rose-400">{Number.isFinite(res.max_drawdown_pct) ? `−${Math.abs(res.max_drawdown_pct).toFixed(1)}%` : '—'}</p>
					</div>
					<div class="bg-bg-card border border-border rounded-xl p-4">
						<p class="text-xs text-text-secondary">Sharpe</p>
						<p class="text-xl font-semibold font-mono tabular-nums mt-1 text-text">{Number.isFinite(res.sharpe_ratio) ? res.sharpe_ratio.toFixed(2) : '—'}</p>
					</div>
					<div class="bg-bg-card border border-border rounded-xl p-4">
						<p class="text-xs text-text-secondary">Trades taken</p>
						<p class="text-xl font-semibold font-mono tabular-nums mt-1 text-text">{res.trades_taken}</p>
						{#if res.open_at_end > 0}<p class="text-xs font-mono tabular-nums text-amber-400 mt-0.5">+{res.open_at_end} still open</p>{/if}
					</div>
					<div class="bg-bg-card border border-border rounded-xl p-4">
						<p class="text-xs text-text-secondary">Avg hold</p>
						<p class="text-xl font-semibold font-mono tabular-nums mt-1 text-text">{Number.isFinite(res.avg_holding_days) ? res.avg_holding_days.toFixed(1) : '—'}<span class="text-xs text-text-muted font-normal font-sans"> days</span></p>
					</div>
					{#if res.tickers_signalled > 0}
						<div class="bg-bg-card border border-border rounded-xl p-4">
							<p class="text-xs text-text-secondary">Price coverage</p>
							<p class="text-xl font-semibold font-mono tabular-nums mt-1 text-text">{res.tickers_tradable} <span class="text-xs text-text-muted font-normal">of {res.tickers_signalled}</span></p>
							<p class="text-xs text-text-muted mt-0.5">signalled tickers had prices</p>
						</div>
					{/if}
				</div>

				{#if (res.equity_curve?.length ?? 0) > 1}
					<div class="bg-bg-card border border-border rounded-xl p-5">
						<h2 class="text-sm font-semibold text-text mb-3">Equity</h2>
						<EquityChart curve={res.equity_curve ?? []} spy={loadedFromHistory ? [] : (run?.spy_curve ?? [])} baseline={res.starting_equity} />
					</div>
				{/if}

				<div class="grid gap-5 lg:grid-cols-[1.6fr_1fr]">
					{#if (res.monthly_returns?.length ?? 0) > 0}
						<div class="bg-bg-card border border-border rounded-xl p-5 min-w-0">
							<h2 class="text-sm font-semibold text-text mb-3">Monthly returns</h2>
							<MonthlyBars months={res.monthly_returns ?? []} />
						</div>
					{/if}
					{#if reasonCounts.length > 0}
						<div class="bg-bg-card border border-border rounded-xl p-5 min-w-0 {(res.monthly_returns?.length ?? 0) === 0 ? 'lg:col-span-2' : ''}">
							<h2 class="text-sm font-semibold text-text mb-3">How trades ended</h2>
							<ul class="space-y-2">
								{#each reasonCounts as [reason, n] (reason)}
									<li class="flex items-center gap-3 text-xs">
										<span class="w-36 shrink-0 text-text-secondary">{exitLabel(reason)}</span>
										<span class="flex-1 h-1.5 rounded-full bg-bg overflow-hidden"><span class="block h-full rounded-full bg-text-muted" style="width:{(n / reasonCounts[0][1]) * 100}%"></span></span>
										<span class="font-mono tabular-nums text-text w-8 text-right">{n}</span>
									</li>
								{/each}
							</ul>
						</div>
					{/if}
				</div>

				<div class="bg-bg-card border border-border rounded-xl p-5">
					<h2 class="text-sm font-semibold text-text mb-3">Trades <span class="text-text-muted font-normal font-mono tabular-nums">{res.trades.length}</span></h2>
					{#if res.trades.length === 0}
						<p class="text-xs text-text-muted">No trades closed. Positions may still be open at the end of the data.</p>
					{:else}
						<TradesTable trades={res.trades} />
					{/if}
				</div>
			</div>
			{#if running}
				<div class="absolute inset-x-0 top-24 flex justify-center pointer-events-none">
					<p class="bg-bg-card border border-border rounded-lg px-4 py-2 text-sm text-text-secondary shadow-lg"><span class="inline-block w-3 h-3 mr-2 align-[-1px] rounded-full border-2 border-ai border-t-transparent animate-spin"></span>Running…</p>
				</div>
			{/if}
		{/if}
	</section>

	<!-- Past runs -->
	<section class="bg-bg-card border border-border rounded-xl p-5" aria-labelledby="bt-past">
		<h2 id="bt-past" class="text-sm font-semibold text-text mb-3">Past runs</h2>
		{#if historyLoading}
			<div class="space-y-2" aria-hidden="true">
				{#each [0, 1, 2] as i (i)}<div class="h-8 rounded bg-bg-card-hover animate-pulse"></div>{/each}
			</div>
		{:else if history.length === 0}
			<p class="text-xs text-text-muted">No saved runs yet. Each run you make is kept here.</p>
		{:else}
			<ul class="divide-y divide-border-subtle">
				{#each history.slice(0, 10) as h, i (i)}
					<li>
						<button type="button" onclick={() => loadPast(h)} class="w-full flex items-center gap-4 py-2 text-left rounded hover:bg-bg-card-hover transition-colors duration-150 focus-visible:outline-2 focus-visible:outline-ai">
							<span class="flex-1 min-w-0 truncate text-xs text-text-secondary" title={h.config_summary}>{h.config_summary}</span>
							<span class="w-16 text-right font-mono tabular-nums text-xs {tone(h.total_return_pct)}">{pct(h.total_return_pct)}</span>
							<span class="w-16 shrink-0 whitespace-nowrap text-right font-mono tabular-nums text-xs text-text-secondary">{Number.isFinite(h.hit_rate) ? h.hit_rate.toFixed(0) : '—'}% hit</span>
							<span class="w-20 shrink-0 whitespace-nowrap text-right font-mono tabular-nums text-xs text-text-secondary">{h.trades_taken} trades</span>
							<span class="w-24 shrink-0 whitespace-nowrap text-right font-mono tabular-nums text-xs text-text-secondary">Sharpe {Number.isFinite(h.sharpe_ratio) ? h.sharpe_ratio.toFixed(2) : '—'}</span>
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	</section>
</div>
