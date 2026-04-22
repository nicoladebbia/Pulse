<script lang="ts">
	import { SECTORS, SECTOR_ORDER, type SectorId } from '$lib/config';
	import { activeSectors, currentBriefing } from '$lib/stores/briefing';
	import { page } from '$app/stores';
	import { isTauri, mockBriefingData, mockUsageStats, mockTavilyQuota } from '$lib/tauri/mock';
	import type { UsageStats, TavilyQuota, IntelligenceCounts, FinancialApiQuota } from '$lib/tauri/types';
	import { isFetching, fetchDone, fetchError, triggerFetch as storeTriggerFetch } from '$lib/stores/fetch';
	import FetchTaskList from '$lib/components/FetchTaskList.svelte';

	let usageStats = $state<UsageStats | null>(null);
	let tavilyQuota = $state<TavilyQuota | null>(null);
	let financialQuotas = $state<FinancialApiQuota[]>([]);
	let intel = $state<IntelligenceCounts | null>(null);
	let showUsageDetail = $state(false);
	let showFinancialApis = $state(false);
	let prevCost = $state(0);
	let costAnimating = $state(false);

	// Load usage on mount. Poll rate: 3s during fetch, 30s idle.
	let usageInitDone = $state(false);

	$effect(() => {
		if (usageInitDone) return;
		usageInitDone = true;
		loadUsage();
	});

	// Reactive poll rate based on fetch state
	$effect(() => {
		const rate = $isFetching ? 3000 : 30000;
		const handle = setInterval(loadUsage, rate);
		return () => clearInterval(handle);
	});

	async function loadUsage() {
		try {
			if (!isTauri()) {
				usageStats = mockUsageStats;
				tavilyQuota = mockTavilyQuota;
				return;
			}
			const ipc = (window as any).__TAURI_INTERNALS__;
			const [usage, quota, finQuotas] = await Promise.all([
				ipc.invoke('get_api_usage', { days: 30 }),
				ipc.invoke('get_tavily_quota'),
				ipc.invoke('get_financial_quotas').catch(() => []),
			]);
			// Animate if cost increased
			if (usageStats && usage.total_cost_usd > usageStats.total_cost_usd) {
				prevCost = usageStats.total_cost_usd;
				costAnimating = true;
				setTimeout(() => { costAnimating = false; }, 1500);
			}
			usageStats = usage;
			tavilyQuota = quota;
			financialQuotas = finQuotas.filter((q: FinancialApiQuota) => q.calls_today > 0 || q.limit_per_minute > 0);
			// Also load intelligence counts
			ipc.invoke('get_intelligence_counts').then((c: IntelligenceCounts) => { intel = c; }).catch(() => {});
		} catch (_) {}
	}

	function toggleSector(id: SectorId) {
		activeSectors.update(current => {
			if (current.includes(id)) {
				return current.filter(s => s !== id);
			} else {
				return [...current, id];
			}
		});
	}

	function selectAll() {
		activeSectors.set([]);
	}

	function getSectorCount(sectorId: SectorId): number {
		const b = $currentBriefing?.briefing;
		if (!b) return 0;
		const countMap: Record<SectorId, number> = {
			ai: b.ai_count,
			miami: b.miami_count,
			italy: b.italy_count,
			tech: b.tech_count,
		};
		return countMap[sectorId] ?? 0;
	}

	const navItems = [
		{ href: '/', label: 'Today', icon: '◉' },
		{ href: '/archive', label: 'Archive', icon: '◫' },
		{ href: '/trends', label: 'Trends', icon: '◈' },
		{ href: '/signals', label: 'Signals', icon: '⬡' },
		{ href: '/freedoms', label: 'Freedoms', icon: '◇' },
		{ href: '/predictions', label: 'Predictions', icon: '⊕' },
		{ href: '/ask', label: 'Ask Pulse', icon: '◎' },
	];

	function handleFetch() {
		storeTriggerFetch();
	}

	async function reloadBriefing() {
		if (!isTauri()) {
			currentBriefing.set(mockBriefingData);
			return;
		}
		try {
			const ipc = (window as any).__TAURI_INTERNALS__;
			const result = await ipc.invoke('get_today_briefing');
			currentBriefing.set(result);
		} catch (_) {}
	}

	// Clock ticker to keep "Xh ago" and next-fetch countdown fresh without reload
	let now = $state(Date.now());
	$effect(() => {
		const h = setInterval(() => { now = Date.now(); }, 60_000);
		return () => clearInterval(h);
	});

	function formatTimeAgo(ts: number, nowMs: number): string {
		const diffMin = Math.floor((nowMs - ts) / 60_000);
		if (diffMin < 1) return 'just now';
		if (diffMin < 60) return `${diffMin}m ago`;
		const diffHr = Math.floor(diffMin / 60);
		if (diffHr < 24) return `${diffHr}h ago`;
		const diffDay = Math.floor(diffHr / 24);
		return `${diffDay}d ago`;
	}

	// Launchd schedules daily fetches at 8:00 AM and 9:00 PM local
	function nextFetchLabel(nowMs: number): string {
		const d = new Date(nowMs);
		const hour = d.getHours();
		const minute = d.getMinutes();
		const currentMin = hour * 60 + minute;
		const schedules = [8 * 60, 21 * 60];
		for (const s of schedules) {
			if (s > currentMin) {
				const h = Math.floor(s / 60);
				const m = s % 60;
				return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
			}
		}
		return '08:00'; // tomorrow
	}

	let lastFetchedMs = $derived(
		$currentBriefing?.briefing.created_at
			? new Date($currentBriefing.briefing.created_at + 'Z').getTime()
			: null
	);
	let lastFetchedLabel = $derived(
		lastFetchedMs ? formatTimeAgo(lastFetchedMs, now) : null
	);
	let nextFetchAt = $derived(nextFetchLabel(now));
</script>

<aside class="w-56 bg-bg-sidebar border-r border-border flex flex-col shrink-0 h-full overflow-y-auto overflow-x-hidden">
	<!-- Logo -->
	<div class="px-5 py-5 border-b border-border">
		<div class="flex items-center gap-2.5">
			<div class="w-3 h-3 rounded-full bg-ai animate-pulse"></div>
			<span class="text-lg font-semibold tracking-tight text-text">Pulse</span>
		</div>
	</div>

	<!-- Navigation -->
	<nav class="px-3 py-4 space-y-1">
		{#each navItems as item}
			<a
				href={item.href}
				class="flex items-center gap-3 px-3 py-2 rounded-lg text-sm transition-colors
					{$page.url.pathname === item.href
						? 'bg-bg-card text-text'
						: 'text-text-secondary hover:bg-bg-card hover:text-text'}"
			>
				<span class="text-base">{item.icon}</span>
				{item.label}
			</a>
		{/each}
	</nav>

	<!-- Refresh Briefing + fetch times -->
	<div class="px-3 py-3 border-t border-border">
		{#if $isFetching}
			<FetchTaskList compact />
			{#if $fetchDone}
				<button class="w-full text-[10px] text-center mt-2 text-ai hover:underline cursor-pointer" onclick={reloadBriefing}>
					Reload briefing
				</button>
			{/if}
		{:else}
			<button
				class="w-full flex items-center justify-center gap-2 px-3 py-2 rounded-lg text-sm
					transition-colors border border-border
					text-text-secondary hover:bg-bg-card hover:text-text hover:border-ai/30"
				onclick={handleFetch}
			>
				<span>↻</span>
				<span>Refresh Briefing</span>
			</button>
			{#if $fetchDone}
				<button class="w-full text-[10px] text-center mt-1.5 text-ai hover:underline cursor-pointer" onclick={reloadBriefing}>
					Done — reload briefing
				</button>
			{/if}
			{#if $fetchError}
				<p class="text-[10px] text-center mt-1.5 text-red-400">{$fetchError}</p>
			{/if}
		{/if}

		<!-- Last fetched / next fetch -->
		<div class="mt-2 px-1 flex items-center justify-center gap-1.5 text-[10px] text-text-muted">
			{#if lastFetchedLabel}
				<span>Last {lastFetchedLabel}</span>
				<span class="opacity-40">•</span>
			{/if}
			<span>Next {nextFetchAt}</span>
		</div>
	</div>

	<!-- Sector filters -->
	<div class="px-3 py-4 border-t border-border">
		<p class="px-3 text-xs font-medium text-text-muted uppercase tracking-wider mb-3">Sectors</p>
		<button
			class="w-full flex items-center gap-3 px-3 py-2 rounded-lg text-sm transition-colors mb-1
				{$activeSectors.length === 0
					? 'bg-bg-card text-text'
					: 'text-text-secondary hover:bg-bg-card hover:text-text'}"
			onclick={selectAll}
		>
			<span class="w-2 h-2 rounded-full bg-text-secondary"></span>
			All <span class="ml-auto text-xs text-text-muted">{$currentBriefing?.briefing.story_count ?? 0}</span>
		</button>
		{#each SECTOR_ORDER as id}
			{@const sector = SECTORS[id]}
			{@const isActive = $activeSectors.length === 0 || $activeSectors.includes(id)}
			<button
				class="w-full flex items-center gap-3 px-3 py-2 rounded-lg text-sm transition-colors mb-1
					{$activeSectors.includes(id)
						? 'bg-bg-card text-text'
						: 'text-text-secondary hover:bg-bg-card hover:text-text'}"
				onclick={() => toggleSector(id)}
			>
				<span class="w-2 h-2 rounded-full" style="background: {isActive ? sector.color : sector.dimColor}"></span>
				{sector.name} <span class="ml-auto text-xs text-text-muted">{getSectorCount(id)}</span>
			</button>
		{/each}
	</div>

	<!-- Intelligence Summary -->
	{#if intel && (intel.entity_count > 0 || intel.active_prediction_count > 0)}
		<div class="px-3 py-4 border-t border-border">
			<p class="px-3 text-xs font-medium text-text-muted uppercase tracking-wider mb-3">Intelligence</p>
			<div class="space-y-1.5 px-3">
				<div class="flex items-center justify-between">
					<span class="text-xs text-text-muted">Entities tracked</span>
					<span class="text-xs font-semibold font-mono text-text">{intel.entity_count}</span>
				</div>
				<div class="flex items-center justify-between">
					<span class="text-xs text-text-muted">Active predictions</span>
					<span class="text-xs font-semibold font-mono text-text">{intel.active_prediction_count}</span>
				</div>
				<div class="flex items-center justify-between">
					<span class="text-xs text-text-muted">Hot signals</span>
					<span class="text-xs font-semibold font-mono text-ai">{intel.hot_signal_count}</span>
				</div>
			</div>
		</div>
	{/if}

	<!-- Spacer to push footer down when sidebar is short -->
	<div class="flex-1"></div>

	<!-- Bottom section: Spend + keyboard hints -->
	<div class="border-t border-border shrink-0">
		<!-- API Usage section -->
		<div class="px-3 py-4 border-t border-border">
			<p class="px-3 text-xs font-medium text-text-muted uppercase tracking-wider mb-3">Spend</p>

			{#if usageStats}
				<!-- Today + Monthly -->
				<div class="px-3 mb-2 space-y-1.5">
					<div class="flex items-center justify-between">
						<span class="text-xs text-text-muted">Today</span>
						<span class="text-xs font-semibold font-mono transition-all duration-700 {costAnimating ? 'text-ai cost-bump' : 'text-text'}">
							${usageStats.today_cost_usd.toFixed(3)}
						</span>
					</div>
					<div class="flex items-center justify-between">
						<span class="text-xs text-text-muted">This month</span>
						<span class="text-xs font-medium font-mono text-text-secondary">
							${usageStats.total_cost_usd.toFixed(2)}
						</span>
					</div>
				</div>

				<!-- Daily cost sparkline -->
				{#if usageStats.daily.length > 1}
					{@const maxCost = Math.max(...usageStats.daily.map(d => d.cost), 0.01)}
					<div class="px-3 mb-2">
						<div class="flex items-end gap-px h-6">
							{#each usageStats.daily as day}
								{@const height = Math.max(2, (day.cost / maxCost) * 24)}
								<div
									class="flex-1 rounded-sm bg-ai/40 hover:bg-ai/70 transition-colors"
									style="height: {height}px"
									title="{day.date}: ${day.cost.toFixed(3)}"
								></div>
							{/each}
						</div>
						<div class="flex justify-between text-[9px] text-text-muted mt-0.5">
							<span>{usageStats.daily.length}d</span>
							<span>{usageStats.total_calls} calls</span>
						</div>
					</div>
				{/if}

				<!-- Tavily quota bar -->
				{#if tavilyQuota}
					{@const pct = Math.round((tavilyQuota.used / tavilyQuota.limit) * 100)}
					{@const barColor = tavilyQuota.remaining === 0 ? 'bg-rose-400' : pct >= 95 ? 'bg-amber-400' : 'bg-ai'}
					<div class="px-3 mb-2">
						<div class="flex items-center justify-between text-[10px] text-text-muted mb-1">
							<span>Web search</span>
							<span class="font-mono">{tavilyQuota.used}/{tavilyQuota.limit}</span>
						</div>
						<div class="w-full h-1 bg-border rounded-full overflow-hidden">
							<div class="h-full {barColor} rounded-full transition-all duration-700" style="width: {pct}%"></div>
						</div>
					</div>
				{/if}

				<!-- Financial APIs section -->
				{#if financialQuotas.length > 0}
					<button
						class="w-full px-3 py-1.5 text-[10px] text-text-muted hover:text-text transition-colors text-left"
						onclick={() => { showFinancialApis = !showFinancialApis; }}
					>
						{showFinancialApis ? '▾ Hide external APIs' : '▸ External APIs'} <span class="font-mono text-text-secondary">{financialQuotas.reduce((s, q) => s + q.calls_today, 0)} calls today</span>
					</button>

					{#if showFinancialApis}
						<div class="px-3 pt-1 space-y-1.5 mb-2">
							{#each financialQuotas as q}
								{@const hasLimit = q.limit_per_minute > 0 || q.limit_per_day > 0}
								{@const hourPct = q.limit_per_minute > 0 ? Math.round((q.calls_this_hour / (q.limit_per_minute * 60)) * 100) : 0}
								<div class="space-y-0.5">
									<div class="flex items-center justify-between text-[10px]">
										<span class="text-text-muted truncate mr-1">{q.description}</span>
										<span class="font-mono text-text-secondary shrink-0">{q.calls_today}</span>
									</div>
									{#if hasLimit}
										{@const barColor = hourPct > 80 ? 'bg-amber-400' : 'bg-emerald-400/60'}
										<div class="w-full h-0.5 bg-border rounded-full overflow-hidden">
											<div class="h-full {barColor} rounded-full transition-all" style="width: {Math.min(hourPct, 100)}%"></div>
										</div>
									{/if}
								</div>
							{/each}
						</div>
					{/if}
				{/if}

				<!-- Expandable provider detail -->
				<button
					class="w-full px-3 py-1.5 text-[10px] text-text-muted hover:text-text transition-colors text-left"
					onclick={() => { showUsageDetail = !showUsageDetail; }}
				>
					{showUsageDetail ? '▾ Hide breakdown' : '▸ Provider breakdown'}
				</button>

				{#if showUsageDetail}
					<div class="px-3 pt-1 space-y-1">
						{#each usageStats.by_provider as p}
							<div class="flex items-center justify-between text-[10px]">
								<span class="text-text-muted truncate mr-2">{p.provider}/{p.model}</span>
								<span class="text-text-secondary font-mono shrink-0">${p.total_cost_usd.toFixed(3)}</span>
							</div>
						{/each}
					</div>
				{/if}
			{/if}
		</div>
	</div>
</aside>

<style>
	:global(.cost-bump) {
		animation: costBump 1.5s ease-out;
	}

	@keyframes costBump {
		0% { transform: translateY(0); }
		15% { transform: translateY(-4px); color: var(--color-ai); }
		30% { transform: translateY(0); }
		100% { color: var(--color-text); }
	}
</style>
