<script lang="ts">
	import { onMount } from 'svelte';
	import StoryCard from '$lib/components/stories/StoryCard.svelte';
	import StoryExpanded from '$lib/components/stories/StoryExpanded.svelte';
	import { SECTORS, type SectorId } from '$lib/config';
	import type { BriefingWithStories, Briefing, Story } from '$lib/tauri/types';

	let briefings = $state<Briefing[]>([]);
	let selectedBriefing = $state<BriefingWithStories | null>(null);
	let expandedStory = $state<Story | null>(null);
	let loading = $state(true);

	onMount(async () => {
		loading = false;
		// Will load from Tauri when available
	});

	function selectDate(date: string) {
		// Will call getBriefingByDate
	}
</script>

<div class="space-y-6 pt-2">
	<div class="flex items-center justify-between">
		<h2 class="text-xl font-semibold text-text">Archive</h2>
	</div>

	{#if expandedStory}
		<StoryExpanded story={expandedStory} onClose={() => expandedStory = null} />
	{:else if selectedBriefing}
		<div class="mb-4">
			<button class="text-sm text-text-muted hover:text-text" onclick={() => selectedBriefing = null}>
				← Back to archive
			</button>
			<h3 class="text-lg font-semibold text-text mt-2">
				{new Date(selectedBriefing.briefing.date + 'T12:00:00').toLocaleDateString('en-US', {
					weekday: 'long', year: 'numeric', month: 'long', day: 'numeric'
				})}
			</h3>
			<p class="text-sm text-text-secondary">{selectedBriefing.briefing.story_count} stories</p>
		</div>
		<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
			{#each selectedBriefing.stories as story (story.id)}
				<StoryCard {story} onExpand={(s) => expandedStory = s} />
			{/each}
		</div>
	{:else if briefings.length > 0}
		<div class="space-y-2">
			{#each briefings as briefing (briefing.id)}
				<button
					class="w-full text-left bg-bg-card border border-border rounded-lg p-4
						hover:bg-bg-card-hover transition-colors flex items-center justify-between"
					onclick={() => selectDate(briefing.date)}
				>
					<div>
						<p class="text-sm font-medium text-text">
							{new Date(briefing.date + 'T12:00:00').toLocaleDateString('en-US', {
								weekday: 'long', year: 'numeric', month: 'long', day: 'numeric'
							})}
						</p>
						<p class="text-xs text-text-muted mt-1">{briefing.story_count} stories</p>
					</div>
					<div class="flex gap-2">
						{#each ['ai', 'miami', 'italy', 'tech'] as sector}
							{@const count = sector === 'ai' ? briefing.ai_count : sector === 'miami' ? briefing.miami_count : sector === 'italy' ? briefing.italy_count : briefing.tech_count}
							<span
								class="text-xs px-1.5 py-0.5 rounded"
								style="color: {SECTORS[sector as SectorId].color}; background: {SECTORS[sector as SectorId].dimColor}"
							>
								{count}
							</span>
						{/each}
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
