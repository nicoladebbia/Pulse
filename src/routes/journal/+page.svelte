<script lang="ts">
	import { isTauri } from '$lib/tauri/mock';
	import { getPaperTrades, getAutoTradeStatus, autoBacktestIfDue, type AutoBacktestStatus, type AutoTradeStatus } from '$lib/tauri/commands';
	import type { PaperTrade } from '$lib/tauri/types';

	let openTrades = $state<PaperTrade[]>([]);
	let closedTrades = $state<PaperTrade[]>([]);
	let autoTradeStatus = $state<AutoTradeStatus | null>(null);
	let backtestStatus = $state<AutoBacktestStatus | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let loadStarted = false;

	// onMount never fires with this app's ssr=false + prerender=true config — see +layout.svelte.
	$effect(() => {
		if (loadStarted) return;
		loadStarted = true;
		loadJournal();
	});

	async function loadJournal() {
		if (!isTauri()) {
			loading = false;
			return;
		}
		try {
			const [open, closed, ats, bs] = await Promise.all([
				getPaperTrades('open'),
				getPaperTrades('closed').catch(() => [] as PaperTrade[]),
				getAutoTradeStatus(),
				autoBacktestIfDue(),
			]);
			// Also fetch stopped_out and expired so the journal shows them.
			const stopped = await getPaperTrades('stopped_out').catch(() => [] as PaperTrade[]);
			const expired = await getPaperTrades('expired').catch(() => [] as PaperTrade[]);
			openTrades = open;
			closedTrades = [...closed, ...stopped, ...expired].sort((a, b) =>
				(b.exit_date ?? b.entry_date).localeCompare(a.exit_date ?? a.entry_date)
			);
			autoTradeStatus = ats;
			backtestStatus = bs;
		} catch (e: any) {
			error = String(e?.message ?? e);
		} finally {
			loading = false;
		}
	}

	interface SignalProfile {
		insider?: number;
		institutional?: number;
		news?: number;
		government?: number;
		search?: number;
		patent?: number;
		supply_chain?: number;
		political?: number;
		stories?: { id: number; headline: string; source: string }[];
	}

	function parseProfile(s: string): SignalProfile {
		try {
			return JSON.parse(s);
		} catch {
			return {};
		}
	}

	function topSignals(p: SignalProfile): { name: string; value: number }[] {
		const dims = [
			['insider', p.insider],
			['institutional', p.institutional],
			['news', p.news],
			['government', p.government],
			['search', p.search],
			['patent', p.patent],
			['supply_chain', p.supply_chain],
			['political', p.political],
		] as const;
		return dims
			.filter(([, v]) => (v ?? 0) > 0.05)
			.map(([name, v]) => ({ name, value: v as number }))
			.sort((a, b) => b.value - a.value)
			.slice(0, 4);
	}

	function fmtPnl(v: number | null): string {
		if (v === null) return '—';
		return (v >= 0 ? '+' : '') + '$' + v.toFixed(2);
	}

	function fmtPct(v: number | null): string {
		if (v === null) return '—';
		return (v >= 0 ? '+' : '') + v.toFixed(2) + '%';
	}

	function statusLabel(s: string): { text: string; cls: string } {
		switch (s) {
			case 'open': return { text: 'Open', cls: 'bg-blue-500/15 text-blue-400' };
			case 'closed': return { text: 'Closed (profit)', cls: 'bg-emerald-500/15 text-emerald-400' };
			case 'stopped_out': return { text: 'Stopped out', cls: 'bg-rose-500/15 text-rose-400' };
			// "(90d)" named a calendar expiry that was removed from the exit engine
			// on 2026-07-15. Nothing writes this status now; the badge stays so an
			// older database still renders, minus the rule that no longer exists.
			case 'expired': return { text: 'Expired (legacy)', cls: 'bg-amber-500/15 text-amber-400' };
			default: return { text: s, cls: 'bg-bg-card text-text-muted' };
		}
	}
</script>

<div class="space-y-6 pt-2 max-w-5xl">
	<header>
		<h2 class="text-xl font-semibold text-text">Trade Journal</h2>
		<p class="text-xs text-text-muted mt-0.5">
			Why each trade was made — entry signals, story sources, exit reason. Read-only.
		</p>
	</header>

	{#if autoTradeStatus}
		<div class="rounded-xl border p-3 text-xs flex items-start gap-3
			{autoTradeStatus.enabled
				? 'bg-rose-500/5 border-rose-500/30 text-rose-300'
				: 'bg-emerald-500/5 border-emerald-500/30 text-emerald-300'}">
			<span class="font-mono shrink-0 mt-0.5">{autoTradeStatus.enabled ? '◉ LIVE' : '◯ OFF'}</span>
			<span>{autoTradeStatus.reason}</span>
		</div>
	{/if}

	{#if backtestStatus}
		<div class="rounded-xl border border-border bg-bg-card p-3 text-xs">
			<div class="flex items-center gap-2">
				<span class="font-mono text-text-muted">Auto-backtest:</span>
				<span class="text-text">{backtestStatus.message}</span>
			</div>
			{#if !backtestStatus.ran_today && backtestStatus.days_of_data < backtestStatus.threshold_days}
				<div class="mt-2 h-1.5 w-full bg-border rounded overflow-hidden">
					<div class="h-full bg-ai/60" style="width: {(backtestStatus.days_of_data / backtestStatus.threshold_days) * 100}%"></div>
				</div>
			{/if}
		</div>
	{/if}

	{#if loading}
		<div class="text-text-muted text-sm">Loading…</div>
	{:else if error}
		<div class="text-rose-400 text-sm">Error: {error}</div>
	{:else}
		{#if openTrades.length === 0 && closedTrades.length === 0}
			<div class="text-center text-text-muted text-sm py-12">
				No trades recorded yet. With the auto-trader off, trades only appear when you place them manually.
			</div>
		{/if}

		{#if openTrades.length > 0}
			<section>
				<h3 class="text-sm font-semibold text-text mb-2 uppercase tracking-wider">Open ({openTrades.length})</h3>
				<div class="space-y-3">
					{#each openTrades as t}
						{@const p = parseProfile(t.signal_profile)}
						{@const top = topSignals(p)}
						{@const slabel = statusLabel(t.status)}
						<article class="bg-bg-card border border-border rounded-xl p-4">
							<div class="flex items-start justify-between gap-3 mb-2">
								<div>
									<div class="flex items-center gap-2">
										<h4 class="text-base font-semibold text-text">{t.ticker}</h4>
										<span class="text-[10px] px-1.5 py-0.5 rounded {slabel.cls}">{slabel.text}</span>
										<span class="text-[10px] text-text-muted">entered {t.entry_date.split('T')[0]}</span>
									</div>
									<div class="text-xs text-text-muted mt-0.5">
										entry ${t.entry_price.toFixed(2)} · size ${t.position_size.toFixed(0)} · score {t.confidence.toFixed(2)}
									</div>
								</div>
								<div class="text-right shrink-0">
									<div class="text-sm font-mono {(t.pnl ?? 0) >= 0 ? 'text-emerald-400' : 'text-rose-400'}">
										{fmtPnl(t.pnl)}
									</div>
									<div class="text-[10px] text-text-muted">{fmtPct(t.pnl_pct)}</div>
								</div>
							</div>

							{#if top.length > 0}
								<div class="flex flex-wrap gap-1.5 mb-2">
									{#each top as s}
										<span class="text-[10px] px-1.5 py-0.5 rounded bg-bg border border-border text-text-secondary font-mono">
											{s.name} {(s.value * 100).toFixed(0)}%
										</span>
									{/each}
								</div>
							{/if}

							{#if p.stories && p.stories.length > 0}
								<details class="text-xs mt-2">
									<summary class="cursor-pointer text-text-muted hover:text-text">
										{p.stories.length} stor{p.stories.length === 1 ? 'y' : 'ies'} drove this trade
									</summary>
									<ul class="mt-2 space-y-1 pl-3">
										{#each p.stories as story}
											<li class="text-text-secondary">
												<span class="text-text-muted text-[10px]">{story.source}:</span>
												{story.headline}
											</li>
										{/each}
									</ul>
								</details>
							{/if}
						</article>
					{/each}
				</div>
			</section>
		{/if}

		{#if closedTrades.length > 0}
			<section>
				<h3 class="text-sm font-semibold text-text mb-2 uppercase tracking-wider">Closed ({closedTrades.length})</h3>
				<div class="space-y-3">
					{#each closedTrades as t}
						{@const p = parseProfile(t.signal_profile)}
						{@const top = topSignals(p)}
						{@const slabel = statusLabel(t.status)}
						<article class="bg-bg-card border border-border rounded-xl p-4 opacity-90">
							<div class="flex items-start justify-between gap-3 mb-2">
								<div>
									<div class="flex items-center gap-2">
										<h4 class="text-base font-semibold text-text">{t.ticker}</h4>
										<span class="text-[10px] px-1.5 py-0.5 rounded {slabel.cls}">{slabel.text}</span>
										<span class="text-[10px] text-text-muted">{t.entry_date.split('T')[0]} → {(t.exit_date ?? '').split('T')[0]}</span>
									</div>
									<div class="text-xs text-text-muted mt-0.5">
										${t.entry_price.toFixed(2)} → ${(t.exit_price ?? 0).toFixed(2)} · size ${t.position_size.toFixed(0)}
									</div>
								</div>
								<div class="text-right shrink-0">
									<div class="text-sm font-mono {(t.pnl ?? 0) >= 0 ? 'text-emerald-400' : 'text-rose-400'}">
										{fmtPnl(t.pnl)}
									</div>
									<div class="text-[10px] text-text-muted">{fmtPct(t.pnl_pct)}</div>
								</div>
							</div>

							{#if t.trade_journal}
								<p class="text-xs text-text-secondary leading-relaxed mb-2 italic">
									{t.trade_journal}
								</p>
							{/if}

							{#if top.length > 0}
								<div class="flex flex-wrap gap-1.5">
									{#each top as s}
										<span class="text-[10px] px-1.5 py-0.5 rounded bg-bg border border-border text-text-secondary font-mono">
											{s.name} {(s.value * 100).toFixed(0)}%
										</span>
									{/each}
								</div>
							{/if}

							{#if p.stories && p.stories.length > 0}
								<details class="text-xs mt-2">
									<summary class="cursor-pointer text-text-muted hover:text-text">
										{p.stories.length} stor{p.stories.length === 1 ? 'y' : 'ies'} drove this trade
									</summary>
									<ul class="mt-2 space-y-1 pl-3">
										{#each p.stories as story}
											<li class="text-text-secondary">
												<span class="text-text-muted text-[10px]">{story.source}:</span>
												{story.headline}
											</li>
										{/each}
									</ul>
								</details>
							{/if}
						</article>
					{/each}
				</div>
			</section>
		{/if}
	{/if}
</div>
