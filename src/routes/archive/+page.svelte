<script lang="ts">
	import HeroCard from '$lib/components/stories/HeroCard.svelte';
	import StoryCard from '$lib/components/stories/StoryCard.svelte';
	import StoryExpanded from '$lib/components/stories/StoryExpanded.svelte';
	import FreedomSection from '$lib/components/freedoms/FreedomSection.svelte';
	import { SECTORS, type SectorId } from '$lib/config';
	import type { BriefingWithStories, Briefing, Story, FreedomsBriefing } from '$lib/tauri/types';
	import { isTauri, mockArchiveBriefings, mockBriefingData, mockFreedomsBriefing } from '$lib/tauri/mock';

	let briefings = $state<Briefing[]>([]);
	let selectedBriefing = $state<BriefingWithStories | null>(null);
	let selectedFreedoms = $state<FreedomsBriefing | null>(null);
	let expandedStory = $state<Story | null>(null);
	let loading = $state(true);
	let loaded = $state(false);
	let error = $state<string | null>(null);
	let viewMode = $state<'list' | 'daily' | 'freedoms'>('list');

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadArchive();
	});

	async function loadArchive() {
		loading = true;
		error = null;
		try {
			if (!isTauri()) {
				briefings = mockArchiveBriefings;
				return;
			}
			const ipc = (window as any).__TAURI_INTERNALS__;
			briefings = await ipc.invoke('list_briefings');
		} catch (e: any) {
			error = String(e?.message ?? e);
		} finally {
			loading = false;
		}
	}

	async function selectDaily(date: string) {
		try {
			if (!isTauri()) {
				selectedBriefing = mockBriefingData;
				selectedFreedoms = null;
				viewMode = 'daily';
				return;
			}
			const ipc = (window as any).__TAURI_INTERNALS__;
			selectedBriefing = await ipc.invoke('get_briefing_by_date', { date });
			selectedFreedoms = null;
			viewMode = 'daily';
		} catch (e) {
			console.error('Failed to load briefing:', e);
		}
	}

	async function selectFreedoms(date: string) {
		try {
			if (!isTauri()) {
				selectedFreedoms = mockFreedomsBriefing;
				selectedBriefing = null;
				viewMode = 'freedoms';
				return;
			}
			const ipc = (window as any).__TAURI_INTERNALS__;
			selectedFreedoms = await ipc.invoke('get_freedoms_by_date', { date });
			selectedBriefing = null;
			viewMode = 'freedoms';
		} catch (e) {
			console.error('Failed to load freedoms:', e);
		}
	}

	function backToList() {
		viewMode = 'list';
		selectedBriefing = null;
		selectedFreedoms = null;
		expandedStory = null;
	}

	function formatDate(date: string): string {
		return new Date(date + 'T12:00:00').toLocaleDateString('en-US', {
			weekday: 'long', year: 'numeric', month: 'long', day: 'numeric'
		});
	}

	function isToday(date: string): boolean {
		return date === new Date().toISOString().split('T')[0];
	}

	// Group briefings by date — supports multiple daily briefings
	let groupedByDate = $derived(() => {
		const groups: Record<string, { dailies: Briefing[]; freedoms?: Briefing }> = {};
		for (const b of briefings) {
			if (!groups[b.date]) groups[b.date] = { dailies: [] };
			if (b.briefing_type === 'freedoms') groups[b.date].freedoms = b;
			else groups[b.date].dailies.push(b);
		}
		return Object.entries(groups).sort((a, b) => b[0].localeCompare(a[0]));
	});

	const freedomDefs = [
		{ key: 'time_stories' as const, freedom: 'time', label: 'Time Freedom', color: 'var(--color-freedom-time)' },
		{ key: 'financial_stories' as const, freedom: 'financial', label: 'Financial Freedom', color: 'var(--color-freedom-financial)' },
		{ key: 'location_stories' as const, freedom: 'location', label: 'Location Freedom', color: 'var(--color-freedom-location)' },
		{ key: 'health_stories' as const, freedom: 'health', label: 'Health Freedom', color: 'var(--color-freedom-health)' },
	];
</script>

<div class="space-y-6 pt-2">
	<div class="flex items-center justify-between">
		<h2 class="text-xl font-semibold text-text">Archive</h2>
		<p class="text-sm text-text-muted">{groupedByDate().length} day{groupedByDate().length !== 1 ? 's' : ''}</p>
	</div>

	{#if expandedStory}
		<StoryExpanded story={expandedStory} onClose={() => expandedStory = null} />

	{:else if viewMode === 'daily' && selectedBriefing}
		<div class="mb-4">
			<button class="text-sm text-text-muted hover:text-text transition-colors" onclick={backToList}>
				← Back to archive
			</button>
			<div class="flex items-center gap-3 mt-2">
				<h3 class="text-lg font-semibold text-text">
					{formatDate(selectedBriefing.briefing.date)}
				</h3>
				<span class="text-[10px] uppercase tracking-wider bg-ai/10 text-ai px-2 py-0.5 rounded">Daily</span>
			</div>
			<p class="text-sm text-text-secondary mt-1">{selectedBriefing.briefing.story_count} stories</p>
		</div>

		{@const hero = selectedBriefing.stories.find(s => s.is_hero) ?? selectedBriefing.stories[0]}
		{@const grid = selectedBriefing.stories.filter(s => s.id !== hero?.id)}

		{#if hero}
			<HeroCard story={hero} onExpand={(s) => expandedStory = s} />
		{/if}
		{#if grid.length > 0}
			<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
				{#each grid as story (story.id)}
					<StoryCard {story} onExpand={(s) => expandedStory = s} />
				{/each}
			</div>
		{/if}

	{:else if viewMode === 'freedoms' && selectedFreedoms}
		<div class="mb-4">
			<button class="text-sm text-text-muted hover:text-text transition-colors" onclick={backToList}>
				← Back to archive
			</button>
		</div>
		<div class="max-w-2xl mx-auto pb-12">
			<header class="mb-10">
				<p class="text-[10px] uppercase tracking-[0.3em] text-gold-muted mb-3">{formatDate(selectedFreedoms.date)}</p>
				<h3 class="text-xl font-light text-text tracking-wide mb-1">The Four Freedoms</h3>
				<div class="w-10 h-px bg-gold mt-4 opacity-60"></div>
			</header>
			<div class="space-y-12">
				{#each freedomDefs as f}
					{@const stories = selectedFreedoms[f.key]}
					{#if stories.length > 0}
						<FreedomSection freedom={f.freedom} label={f.label} color={f.color} {stories} />
					{/if}
				{/each}
			</div>
		</div>

	{:else if loading}
		<div class="flex items-center justify-center h-48">
			<div class="w-6 h-6 border-2 border-ai border-t-transparent rounded-full animate-spin"></div>
		</div>

	{:else if error}
		<div class="flex items-center justify-center h-48">
			<div class="text-center max-w-md">
				<div class="text-4xl mb-4">⚠</div>
				<h3 class="text-lg font-semibold text-text mb-2">Error Loading Archive</h3>
				<p class="text-text-secondary text-sm mb-4">{error}</p>
				<button class="text-sm text-ai hover:underline" onclick={loadArchive}>Retry</button>
			</div>
		</div>

	{:else if groupedByDate().length > 0}
		<div class="space-y-2">
			{#each groupedByDate() as [date, group]}
				<div class="bg-bg-card border border-border rounded-lg p-4">
					<div class="flex items-center justify-between">
						<div>
							<div class="flex items-center gap-2">
								<p class="text-sm font-medium text-text">{formatDate(date)}</p>
								{#if isToday(date)}
									<span class="text-[10px] uppercase tracking-wider bg-ai/10 text-ai px-1.5 py-0.5 rounded">Today</span>
								{/if}
							</div>
						</div>
						<div class="flex items-center gap-2 flex-wrap">
							{#each group.dailies as daily}
								<button
									class="text-xs px-3 py-1.5 rounded-lg border border-border hover:bg-bg-card-hover
										transition-colors text-text-secondary hover:text-text"
									onclick={() => selectDaily(date)}
								>
									{daily.time_label ?? 'Daily'} · {daily.story_count} stories
								</button>
							{/each}
							{#if group.freedoms}
								<button
									class="text-xs px-3 py-1.5 rounded-lg border transition-colors hover:opacity-90"
									style="border-color: var(--color-gold-dim); color: var(--color-gold); background: var(--color-gold-dim)"
									onclick={() => selectFreedoms(date)}
								>
									Freedoms · {group.freedoms.story_count}
								</button>
							{/if}
						</div>
					</div>

					{#if group.dailies.length > 0}
						{@const totalStories = group.dailies.reduce((s, d) => s + d.story_count, 0)}
						{@const latestDaily = group.dailies[0]}
						<div class="flex gap-1.5 mt-2">
							{#each ['ai', 'miami', 'italy', 'tech'] as sector}
								{@const count = sector === 'ai' ? latestDaily.ai_count : sector === 'miami' ? latestDaily.miami_count : sector === 'italy' ? latestDaily.italy_count : latestDaily.tech_count}
								<span
									class="text-xs font-mono px-1.5 py-0.5 rounded min-w-[24px] text-center"
									style="color: {SECTORS[sector as SectorId].color}; background: {SECTORS[sector as SectorId].dimColor}"
								>
									{count}
								</span>
							{/each}
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{:else}
		<div class="flex items-center justify-center h-48">
			<div class="text-center">
				<div class="text-4xl mb-4">◫</div>
				<h3 class="text-lg font-semibold text-text mb-2">No Archives Yet</h3>
				<p class="text-text-secondary text-sm">Past briefings will appear here after your first fetch.</p>
			</div>
		</div>
	{/if}
</div>
