<script lang="ts">
	import { currentBriefing, isLoading, activeSectors, expandedStoryId, getFilteredStories, getFeaturedFromList, getCompactFromList } from '$lib/stores/briefing';
	import { updateStoryList, focusedStoryId } from '$lib/stores/navigation';
	import BriefingSummary from '$lib/components/stories/BriefingSummary.svelte';
	import FeaturedCard from '$lib/components/stories/FeaturedCard.svelte';
	import CompactStoryRow from '$lib/components/stories/CompactStoryRow.svelte';
	import ConnectionInsight from '$lib/components/stories/ConnectionInsight.svelte';
	import StoryExpanded from '$lib/components/stories/StoryExpanded.svelte';
	import type { Story, BriefingWithStories, StoryTrendBadge } from '$lib/tauri/types';
	import { isTauri, mockBriefingData } from '$lib/tauri/mock';
	import { getStoryTrendBadges, safeInvoke } from '$lib/tauri/commands';
	import { isFetching, fetchDone } from '$lib/stores/fetch';
	import FetchTaskList from '$lib/components/FetchTaskList.svelte';

	let trendBadges = $state<Map<number, StoryTrendBadge>>(new Map());

	let allStories = $derived(getFilteredStories($currentBriefing, $activeSectors));
	let featured = $derived(getFeaturedFromList(allStories, 3));
	let compact = $derived(getCompactFromList(allStories, 3, 12));
	let connections = $derived($currentBriefing?.connections ?? []);
	let executiveSummary = $derived($currentBriefing?.briefing.executive_summary ?? null);
	let expandedStory = $derived(allStories.find(s => s.id === $expandedStoryId) ?? null);
	let remainingCount = $derived(Math.max(0, allStories.length - 15));

	let loadError = $state<string | null>(null);
	let loaded = $state(false);
	function handleRefreshReload() {
		fetchDone.set(false);
		loadBriefing();
	}

	// Keep navigation store in sync with visible stories
	$effect(() => {
		updateStoryList(allStories);
	});

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadBriefing();
	});

	async function loadBriefing() {
		isLoading.set(true);
		loadError = null;

		try {
			if (!isTauri()) {
				currentBriefing.set(mockBriefingData);
				return;
			}

			const briefing = await Promise.race([
				safeInvoke<BriefingWithStories | null>('get_today_briefing'),
				new Promise<null>((_, rej) => setTimeout(() => rej(new Error('Timed out loading briefing')), 10000))
			]);

			currentBriefing.set(briefing);

			// Load trend badges for stories
			if (briefing?.stories?.length) {
				const ids = briefing.stories.map(s => s.id);
				const badges = await safeInvoke<StoryTrendBadge[]>('get_story_trend_badges', { storyIds: ids }, []);
				const map = new Map<number, StoryTrendBadge>();
				for (const b of badges) map.set(b.story_id, b);
				trendBadges = map;
			}
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
			<button class="text-sm text-ai hover:underline" onclick={retry}>Retry</button>
		</div>
	</div>
{:else if !$currentBriefing}
	<div class="flex items-center justify-center h-64">
		<div class="text-center max-w-md">
			<div class="text-4xl mb-4">◉</div>
			<h2 class="text-xl font-semibold text-text mb-2">No Briefing Yet</h2>
			<p class="text-text-secondary leading-relaxed">
				Your daily Pulse briefing hasn't been generated yet.
				It runs automatically at 8:00 AM and 9:00 PM, or you can trigger it manually.
			</p>
		</div>
	</div>
{:else if allStories.length === 0}
	<div class="flex items-center justify-center h-64">
		<div class="text-center max-w-md">
			<div class="text-4xl mb-4">◉</div>
			<h2 class="text-xl font-semibold text-text mb-2">No coverage today</h2>
			<p class="text-text-secondary leading-relaxed">
				{#if $activeSectors.length > 0}
					No stories found for the selected sectors. Try adding more sectors.
				{:else}
					No stories were curated for today's briefing.
				{/if}
			</p>
		</div>
	</div>
{:else}
	<div class="space-y-8 pt-2 max-w-4xl mx-auto">
		<!-- Staleness nudge -->
		{#if $fetchDone && !$isFetching}
			<div class="flex items-center justify-between bg-emerald-400/5 border border-emerald-400/20 rounded-xl px-4 py-3">
				<div class="flex items-center gap-3">
					<div class="w-2 h-2 rounded-full bg-emerald-400"></div>
					<p class="text-sm text-text-secondary">Briefing refreshed successfully.</p>
				</div>
				<button
					class="text-xs px-3 py-1.5 rounded-lg bg-ai/15 text-ai border border-ai/25 hover:bg-ai/25 transition-colors"
					onclick={handleRefreshReload}
				>
					Reload
				</button>
			</div>
		{:else if $isFetching}
			<div class="bg-bg-card border border-ai/20 rounded-xl px-4 py-4">
				<FetchTaskList />
			</div>
		{/if}

		<!-- TIER 1: Executive Summary -->
		<BriefingSummary summary={executiveSummary} briefing={$currentBriefing.briefing} />

		<!-- TIER 2: Featured Stories (top 3 by relevance) -->
		<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
			{#each featured as story (story.id)}
				<FeaturedCard {story} onExpand={handleExpand} focused={story.id === $focusedStoryId} trendBadge={trendBadges.get(story.id)} />
			{/each}
		</div>

		<!-- Cross-Connections -->
		{#if connections.length > 0}
			<div class="space-y-3">
				{#each connections.slice(0, 3) as connection}
					<ConnectionInsight {connection} />
				{/each}
			</div>
		{/if}

		<!-- TIER 3: Also Today -->
		{#if compact.length > 0}
			<div>
				<h3 class="text-xs font-semibold uppercase tracking-wider text-text-muted mb-3">Also Today</h3>
				<div class="bg-bg-card border border-border rounded-xl divide-y divide-border/50">
					{#each compact as story (story.id)}
						<CompactStoryRow {story} onExpand={handleExpand} focused={story.id === $focusedStoryId} trendBadge={trendBadges.get(story.id)} />
					{/each}
				</div>
				{#if remainingCount > 0}
					<a href="/archive" class="block text-center text-sm text-ai hover:underline mt-3 py-2">
						View all {$currentBriefing.briefing.story_count} stories in Archive
					</a>
				{/if}
			</div>
		{/if}
	</div>
{/if}
