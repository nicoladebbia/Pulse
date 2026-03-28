<script lang="ts">
	import { currentBriefing, isLoading, activeSector, expandedStoryId, getFilteredStories } from '$lib/stores/briefing';
	import HeroCard from '$lib/components/stories/HeroCard.svelte';
	import StoryCard from '$lib/components/stories/StoryCard.svelte';
	import StoryExpanded from '$lib/components/stories/StoryExpanded.svelte';
	import type { Story, BriefingWithStories } from '$lib/tauri/types';

	let stories = $derived(getFilteredStories($currentBriefing, $activeSector));
	let heroStory = $derived(stories.find(s => s.is_hero) ?? stories[0] ?? null);
	let gridStories = $derived(heroStory ? stories.filter(s => s.id !== heroStory?.id) : []);
	let expandedStory = $derived(stories.find(s => s.id === $expandedStoryId) ?? null);
	let loadError = $state<string | null>(null);
	let loaded = $state(false);

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadBriefing();
	});

	async function loadBriefing() {
		isLoading.set(true);
		loadError = null;

		try {
			const ipc = (window as any).__TAURI_INTERNALS__;
			if (!ipc) {
				loadError = 'Not running inside Tauri.';
				return;
			}

			const result = await Promise.race([
				ipc.invoke('get_today_briefing'),
				new Promise((_, rej) => setTimeout(() => rej(new Error('Timed out loading briefing')), 10000))
			]);

			currentBriefing.set(result as BriefingWithStories | null);
		} catch (e: any) {
			loadError = String(e?.message ?? e);
		} finally {
			isLoading.set(false);
		}
	}

	function handleExpand(story: Story) {
		expandedStoryId.set(story.id);
	}

	function retry() {
		loadBriefing();
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
{:else if loadError}
	<div class="flex items-center justify-center h-64">
		<div class="text-center max-w-md">
			<div class="text-4xl mb-4">⚠</div>
			<h2 class="text-xl font-semibold text-text mb-2">Error Loading Briefing</h2>
			<p class="text-text-secondary text-sm leading-relaxed mb-4">{loadError}</p>
			<button
				class="text-sm text-ai hover:underline"
				onclick={retry}
			>
				Retry
			</button>
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
		{#if heroStory}
			<HeroCard story={heroStory} onExpand={handleExpand} />
		{/if}

		{#if gridStories.length > 0}
			<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
				{#each gridStories as story (story.id)}
					<StoryCard {story} onExpand={handleExpand} />
				{/each}
			</div>
		{/if}
	</div>
{/if}
