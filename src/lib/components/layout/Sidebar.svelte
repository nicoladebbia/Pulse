<script lang="ts">
	import { SECTORS, SECTOR_ORDER, type SectorId } from '$lib/config';
	import { activeSectors, currentBriefing } from '$lib/stores/briefing';
	import { page } from '$app/stores';
	import { isTauri, mockBriefingData, mockUsageStats, mockTavilyQuota } from '$lib/tauri/mock';
	import type { UsageStats, TavilyQuota } from '$lib/tauri/types';
	import { isFetching, fetchDone, fetchError, triggerFetch as storeTriggerFetch } from '$lib/stores/fetch';
	import FetchTaskList from '$lib/components/FetchTaskList.svelte';

	let usageStats = $state<UsageStats | null>(null);
	let tavilyQuota = $state<TavilyQuota | null>(null);
	let showUsageDetail = $state(false);
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
			const [usage, quota] = await Promise.all([
				ipc.invoke('get_api_usage', { days: 30 }),
				ipc.invoke('get_tavily_quota'),
			]);
			// Animate if cost increased
			if (usageStats && usage.total_cost_usd > usageStats.total_cost_usd) {
				prevCost = usageStats.total_cost_usd;
				costAnimating = true;
				setTimeout(() => { costAnimating = false; }, 1500);
			}
			usageStats = usage;
			tavilyQuota = quota;
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
		{ href: '/freedoms', label: 'Freedoms', icon: '◇' },
		{ href: '/ideas', label: 'Ideas', icon: '~' },
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

	<!-- Spacer to push footer down when sidebar is short -->
	<div class="flex-1"></div>

	<!-- Refetch + keyboard hints -->
	<div class="border-t border-border shrink-0">
		<div class="px-3 py-3">
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
		</div>
		<!-- API Usage section (same style as Sectors) -->
		<div class="px-3 py-4 border-t border-border">
			<p class="px-3 text-xs font-medium text-text-muted uppercase tracking-wider mb-3">API Usage</p>

			<!-- Running total cost — always visible -->
			<div class="px-3 mb-2">
				<div class="flex items-center justify-between">
					<span class="text-sm text-text-secondary">Monthly cost</span>
					<span class="text-sm font-semibold font-mono transition-all duration-700 {costAnimating ? 'text-ai cost-bump' : 'text-text'}">
						${usageStats?.total_cost_usd.toFixed(2) ?? '0.00'}
					</span>
				</div>
			</div>

			<!-- Tavily quota bar -->
			{#if tavilyQuota}
				{@const pct = Math.round((tavilyQuota.used / tavilyQuota.limit) * 100)}
				{@const barColor = tavilyQuota.remaining === 0 ? 'bg-rose-400' : pct >= 95 ? 'bg-amber-400' : 'bg-ai'}
				<div class="px-3 mb-2">
					<div class="flex items-center justify-between text-xs text-text-secondary mb-1">
						<span>Web search</span>
						<span class="font-mono text-text-muted">{tavilyQuota.used}/{tavilyQuota.limit}</span>
					</div>
					<div class="w-full h-1.5 bg-border rounded-full overflow-hidden">
						<div class="h-full {barColor} rounded-full transition-all duration-700" style="width: {pct}%"></div>
					</div>
				</div>
			{/if}

			<!-- Expandable detail -->
			<button
				class="w-full flex items-center gap-3 px-3 py-2 rounded-lg text-sm transition-colors
					text-text-secondary hover:bg-bg-card hover:text-text"
				onclick={() => { showUsageDetail = !showUsageDetail; }}
			>
				<span class="w-2 h-2 rounded-full bg-text-secondary"></span>
				{showUsageDetail ? 'Hide details' : 'Show details'}
			</button>

			{#if showUsageDetail && usageStats}
				<div class="px-3 pt-2 space-y-1.5">
					{#each usageStats.by_provider as p}
						<div class="flex items-center justify-between text-xs">
							<span class="text-text-muted">{p.provider}/{p.model}</span>
							<span class="text-text-secondary font-mono">${p.total_cost_usd.toFixed(3)}</span>
						</div>
					{/each}
					<div class="flex items-center justify-between text-xs pt-1.5 border-t border-border/50">
						<span class="text-text-muted">Tokens in/out</span>
						<span class="text-text-secondary font-mono">{(usageStats.total_input_tokens / 1000).toFixed(0)}k / {(usageStats.total_output_tokens / 1000).toFixed(0)}k</span>
					</div>
					<div class="flex items-center justify-between text-xs">
						<span class="text-text-muted">API calls</span>
						<span class="text-text-secondary font-mono">{usageStats.by_provider.reduce((s, p) => s + p.call_count, 0)}</span>
					</div>
				</div>
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
