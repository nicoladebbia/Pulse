<script lang="ts">
	import HeroCard from '$lib/components/stories/HeroCard.svelte';
	import StoryCard from '$lib/components/stories/StoryCard.svelte';
	import StoryExpanded from '$lib/components/stories/StoryExpanded.svelte';
	import { SECTORS, type SectorId } from '$lib/config';
	import type { BriefingWithStories, Briefing, Story } from '$lib/tauri/types';

	let briefings = $state<Briefing[]>([]);
	let selectedBriefing = $state<BriefingWithStories | null>(null);
	let expandedStory = $state<Story | null>(null);
	let loading = $state(true);
	let loaded = $state(false);

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadArchive();
	});

	async function loadArchive() {
		loading = true;
		try {
			const ipc = (window as any).__TAURI_INTERNALS__;
			if (!ipc) return;
			briefings = await ipc.invoke('list_briefings');
		} catch (e) {
			console.error('Failed to load archive:', e);
		} finally {
			loading = false;
		}
	}

	async function selectDate(date: string) {
		try {
			const ipc = (window as any).__TAURI_INTERNALS__;
			if (!ipc) return;
			selectedBriefing = await ipc.invoke('get_briefing_by_date', { date });
		} catch (e) {
			console.error('Failed to load briefing:', e);
		}
	}

	function formatDate(date: string): string {
		return new Date(date + 'T12:00:00').toLocaleDateString('en-US', {
			weekday: 'long', year: 'numeric', month: 'long', day: 'numeric'
		});
	}

	function isToday(date: string): boolean {
		return date === new Date().toISOString().split('T')[0];
	}
</script>

<div class="space-y-6 pt-2">
	<div class="flex items-center justify-between">
		<h2 class="text-xl font-semibold text-text">Archive</h2>
		<p class="text-sm text-text-muted">{briefings.length} briefing{briefings.length !== 1 ? 's' : ''}</p>
	</div>

	{#if expandedStory}
		<StoryExpanded story={expandedStory} onClose={() => expandedStory = null} />
	{:else if selectedBriefing}
		<!-- Selected day's briefing -->
		<div class="mb-4">
			<button class="text-sm text-text-muted hover:text-text transition-colors" onclick={() => selectedBriefing = null}>
				← Back to archive
			</button>
			<div class="flex items-center gap-3 mt-2">
				<h3 class="text-lg font-semibold text-text">
					{formatDate(selectedBriefing.briefing.date)}
				</h3>
				{#if isToday(selectedBriefing.briefing.date)}
					<span class="text-[10px] uppercase tracking-wider bg-ai/10 text-ai px-2 py-0.5 rounded">Today</span>
				{/if}
			</div>
			<p class="text-sm text-text-secondary mt-1">{selectedBriefing.briefing.story_count} stories</p>
		</div>

		<!-- Hero + grid -->
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
	{:else if loading}
		<div class="flex items-center justify-center h-48">
			<div class="w-6 h-6 border-2 border-ai border-t-transparent rounded-full animate-spin"></div>
		</div>
	{:else if briefings.length > 0}
		<!-- Briefing list -->
		<div class="space-y-2">
			{#each briefings as briefing (briefing.id)}
				<button
					class="w-full text-left bg-bg-card border border-border rounded-lg p-4
						hover:bg-bg-card-hover transition-colors flex items-center justify-between group"
					onclick={() => selectDate(briefing.date)}
				>
					<div>
						<div class="flex items-center gap-2">
							<p class="text-sm font-medium text-text">
								{formatDate(briefing.date)}
							</p>
							{#if isToday(briefing.date)}
								<span class="text-[10px] uppercase tracking-wider bg-ai/10 text-ai px-1.5 py-0.5 rounded">Today</span>
							{/if}
						</div>
						<p class="text-xs text-text-muted mt-1">{briefing.story_count} stories</p>
					</div>
					<div class="flex items-center gap-3">
						<div class="flex gap-1.5">
							{#each ['ai', 'miami', 'italy', 'tech'] as sector}
								{@const count = sector === 'ai' ? briefing.ai_count : sector === 'miami' ? briefing.miami_count : sector === 'italy' ? briefing.italy_count : briefing.tech_count}
								<span
									class="text-xs font-mono px-1.5 py-0.5 rounded min-w-[24px] text-center"
									style="color: {SECTORS[sector as SectorId].color}; background: {SECTORS[sector as SectorId].dimColor}"
								>
									{count}
								</span>
							{/each}
						</div>
						<span class="text-text-muted opacity-0 group-hover:opacity-100 transition-opacity">→</span>
					</div>
				</button>
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
