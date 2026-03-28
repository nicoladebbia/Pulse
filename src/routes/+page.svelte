<script lang="ts">
	import { onMount } from 'svelte';
	import { currentBriefing, isLoading, activeSector, expandedStoryId, getFilteredStories } from '$lib/stores/briefing';
	import HeroCard from '$lib/components/stories/HeroCard.svelte';
	import StoryCard from '$lib/components/stories/StoryCard.svelte';
	import StoryExpanded from '$lib/components/stories/StoryExpanded.svelte';
	import { SECTORS, type SectorId } from '$lib/config';
	import type { Story } from '$lib/tauri/types';

	let stories = $derived(getFilteredStories($currentBriefing, $activeSector));
	let heroStory = $derived(stories.find(s => s.is_hero) ?? stories[0] ?? null);
	let gridStories = $derived(heroStory ? stories.filter(s => s.id !== heroStory?.id) : []);
	let expandedStory = $derived(stories.find(s => s.id === $expandedStoryId) ?? null);

	onMount(async () => {
		isLoading.set(true);
		try {
			// Try to load from Tauri, fall back to empty
			if (window.__TAURI_INTERNALS__) {
				const { getTodayBriefing } = await import('$lib/tauri/commands');
				const briefing = await getTodayBriefing();
				currentBriefing.set(briefing);
			}
		} catch (e) {
			console.warn('Failed to load briefing:', e);
		} finally {
			isLoading.set(false);
		}
	});

	function handleExpand(story: Story) {
		expandedStoryId.set(story.id);
	}
</script>

{#if $expandedStoryId && expandedStory}
	<StoryExpanded story={expandedStory} onClose={() => expandedStoryId.set(null)} />
{:else if $isLoading}
	<div class="flex items-center justify-center h-64">
		<div class="text-center">
			<div class="w-8 h-8 border-2 border-ai border-t-transparent rounded-full animate-spin mx-auto mb-4"></div>
			<p class="text-text-muted">Loading your briefing...</p>
		</div>
	</div>
{:else if !$currentBriefing || stories.length === 0}
	<div class="flex items-center justify-center h-64">
		<div class="text-center max-w-md">
			<div class="text-4xl mb-4">◉</div>
			<h2 class="text-xl font-semibold text-text mb-2">No Briefing Yet</h2>
			<p class="text-text-secondary leading-relaxed">
				Your daily Pulse briefing hasn't been generated yet.
				It runs automatically at 8:00 AM, or you can trigger it manually.
			</p>
		</div>
	</div>
{:else}
	<!-- Magazine Layout -->
	<div class="space-y-6 pt-2">
		<!-- Hero Card -->
		{#if heroStory}
			<HeroCard story={heroStory} onExpand={handleExpand} />
		{/if}

		<!-- Story Grid -->
		{#if gridStories.length > 0}
			{@const sectorGroups = Object.groupBy(gridStories, s => s.sector)}
			<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
				{#each gridStories as story (story.id)}
					<StoryCard {story} onExpand={handleExpand} />
				{/each}
			</div>
		{/if}
	</div>
{/if}
