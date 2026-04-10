<script lang="ts">
	import { goto } from '$app/navigation';
	import { SECTORS, type SectorId } from '$lib/config';
	import { isTauri, mockTrends } from '$lib/tauri/mock';
	import { getTrends } from '$lib/tauri/commands';
	import { expandedStoryId } from '$lib/stores/briefing';
	import type { TrendThread } from '$lib/tauri/types';

	let threads = $state<TrendThread[]>([]);
	let isLoading = $state(true);
	let loaded = $state(false);
	let error = $state<string | null>(null);
	let sectorFilter = $state<string>('all');

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadTrends();
	});

	async function loadTrends() {
		error = null;
		isLoading = true;
		try {
			if (!isTauri()) {
				threads = mockTrends;
				return;
			}
			threads = await getTrends();
		} catch (e: any) {
			error = String(e?.message ?? e);
		} finally {
			isLoading = false;
		}
	}

	const filtered = $derived(
		sectorFilter === 'all'
			? threads
			: threads.filter(t => t.sector === sectorFilter)
	);

	const sectors = ['all', 'ai', 'miami', 'italy', 'tech'] as const;
	const sectorLabels: Record<string, string> = {
		all: 'All',
		ai: 'AI & LLMs',
		miami: 'Miami',
		italy: 'Italy',
		tech: 'Tech',
	};

	const trajectoryMeta: Record<string, { icon: string; label: string; class: string }> = {
		emerging: { icon: '↗', label: 'Emerging', class: 'bg-emerald-500/15 text-emerald-400' },
		growing: { icon: '⬆', label: 'Growing', class: 'bg-blue-500/15 text-blue-400' },
		peaking: { icon: '⬤', label: 'Peaking', class: 'bg-amber-500/15 text-amber-400' },
		declining: { icon: '↘', label: 'Declining', class: 'bg-red-500/15 text-red-400' },
	};

	function navigateToStory(storyId: number) {
		expandedStoryId.set(storyId);
		goto('/');
	}

	function askAbout(entity: string) {
		sessionStorage.setItem('pulse_ask_prefill', `Tell me about ${entity}`);
		goto('/ask');
	}

	function formatDate(dateStr: string): string {
		const date = new Date(dateStr + 'T12:00:00');
		const now = new Date();
		const diffDays = Math.floor((now.getTime() - date.getTime()) / 86400000);
		if (diffDays === 0) return 'Today';
		if (diffDays === 1) return 'Yesterday';
		if (diffDays < 7) return `${diffDays}d ago`;
		return date.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
	}
</script>

<div class="space-y-4 pt-2">
	<!-- Header -->
	<div class="flex items-center justify-between">
		<div>
			<h2 class="text-xl font-semibold text-text">Trend Radar</h2>
			<p class="text-xs text-text-muted mt-0.5">Entities gaining momentum across your archive</p>
		</div>
	</div>

	<!-- Sector filters -->
	<div class="flex gap-1.5">
		{#each sectors as s}
			{@const isActive = sectorFilter === s}
			<button
				class="text-xs px-3 py-1.5 rounded-lg transition-colors {isActive
					? 'bg-ai/15 text-ai border border-ai/30'
					: 'bg-bg-card border border-border text-text-muted hover:text-text hover:bg-bg-card-hover'}"
				onclick={() => sectorFilter = s}
			>
				{sectorLabels[s]}
				{#if s !== 'all'}
					{@const count = threads.filter(t => t.sector === s).length}
					{#if count > 0}
						<span class="ml-1 text-[10px] opacity-60">{count}</span>
					{/if}
				{/if}
			</button>
		{/each}
	</div>

	{#if isLoading}
		<!-- Skeleton loading -->
		<div class="space-y-3">
			{#each Array(5) as _}
				<div class="bg-bg-card border border-border rounded-xl p-4 animate-pulse">
					<div class="flex items-center gap-3">
						<div class="w-24 h-4 bg-border rounded"></div>
						<div class="w-16 h-4 bg-border rounded"></div>
					</div>
					<div class="mt-3 space-y-2">
						<div class="w-full h-3 bg-border rounded"></div>
						<div class="w-3/4 h-3 bg-border rounded"></div>
					</div>
				</div>
			{/each}
		</div>
	{:else if error}
		<div class="flex items-center justify-center h-64">
			<div class="text-center max-w-md">
				<div class="text-4xl mb-4">⚠</div>
				<h3 class="text-lg font-semibold text-text mb-2">Error Loading Trends</h3>
				<p class="text-text-secondary text-sm mb-4">{error}</p>
				<button class="text-sm text-ai hover:underline" onclick={loadTrends}>Retry</button>
			</div>
		</div>
	{:else if filtered.length > 0}
		<div class="space-y-3">
			{#each filtered as thread (thread.id)}
				{@const color = SECTORS[thread.sector as SectorId]?.color ?? 'var(--color-text-muted)'}
				{@const traj = trajectoryMeta[thread.trajectory] ?? trajectoryMeta.emerging}
				{@const maxSpark = Math.max(...thread.sparkline, 1)}

				<div class="bg-bg-card border border-border rounded-xl overflow-hidden">
					<!-- Card header -->
					<div class="px-4 pt-3 pb-2 border-l-2" style="border-left-color: {color}">
						<div class="flex items-center justify-between">
							<div class="flex items-center gap-2.5">
								<button
									class="text-sm font-semibold text-text hover:text-ai transition-colors"
									onclick={() => askAbout(thread.title)}
									title="Ask Pulse about {thread.title}"
								>
									{thread.title}
								</button>
								<span class="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium {traj.class}">
									{traj.icon} {traj.label}
								</span>
							</div>
							<div class="flex items-center gap-3 text-[10px] text-text-muted">
								<span>{thread.mention_count} mentions</span>
								<span>{thread.days_active}d active</span>
							</div>
						</div>

						<!-- Sparkline -->
						<div class="flex items-end gap-px mt-2 h-4">
							{#each thread.sparkline as value, i}
								{@const height = value > 0 ? Math.max(3, (value / maxSpark) * 16) : 0}
								<div
									class="flex-1 rounded-sm transition-all"
									style="height: {height}px; background: {value > 0 ? color : 'transparent'}; opacity: {0.3 + (i / 14) * 0.7}"
									title="{value} mention{value !== 1 ? 's' : ''}"
								></div>
							{/each}
						</div>
					</div>

					<!-- Story timeline -->
					<div class="px-4 pb-3 pt-1">
						<div class="space-y-1">
							{#each thread.points as point}
								<button
									class="w-full flex items-center gap-2.5 text-left py-1 px-2 -mx-2 rounded-lg
										hover:bg-bg-card-hover transition-colors group"
									onclick={() => navigateToStory(point.story_id)}
								>
									<span class="text-[10px] text-text-muted shrink-0 w-16">{formatDate(point.date)}</span>
									<span class="w-1 h-1 rounded-full shrink-0" style="background: {color}"></span>
									<span class="text-xs text-text-secondary group-hover:text-text transition-colors truncate">
										{point.headline}
									</span>
								</button>
							{/each}
						</div>
					</div>
				</div>
			{/each}
		</div>
	{:else}
		<div class="flex items-center justify-center h-64">
			<div class="text-center max-w-md">
				<div class="text-4xl mb-4">◈</div>
				<h3 class="text-lg font-semibold text-text mb-2">
					{#if sectorFilter !== 'all'}
						No trends in {sectorLabels[sectorFilter]}
					{:else}
						Building Your Trend Radar
					{/if}
				</h3>
				<p class="text-text-secondary text-sm leading-relaxed">
					{#if sectorFilter !== 'all'}
						Try selecting "All" to see trends across all sectors.
					{:else}
						Trends appear after a few days of briefings as Pulse detects entities
						that keep showing up across your news archive.
					{/if}
				</p>
			</div>
		</div>
	{/if}
</div>
