<script lang="ts">
	import { goto } from '$app/navigation';
	import { expandedStoryId } from '$lib/stores/briefing';
	import { getStoryHeadlines, safeCall } from '$lib/tauri/commands';
	import { isTauri } from '$lib/tauri/mock';
	import { SECTORS, type SectorId } from '$lib/config';
	import type { StoryHeadline } from '$lib/tauri/types';
	import { trackCitationClick } from '$lib/engagement';
	import { relativeDayLabel } from '$lib/date';

	let { storyIds, citedIds = [] }: { storyIds: number[]; citedIds?: number[] } = $props();

	let headlines = $state<StoryHeadline[]>([]);
	let loaded = $state(false);
	let expanded = $state(false);

	// Prefer what the answer actually cited. `storyIds` is the union of the cited
	// stories with everything retrieved, so showing it made an answer that cited
	// one story display ten "Sources" — the opposite of verifiable. Messages
	// written before the backend recorded citations separately have no citedIds
	// and fall back to the retrieved list, honestly relabelled.
	const cited = $derived(citedIds.filter(id => id > 0));
	const showingCited = $derived(cited.length > 0);
	const validIds = $derived(showingCited ? cited : storyIds.filter(id => id > 0));

	function relativeDate(dateStr: string): string {
		return relativeDayLabel(dateStr);
	}
	const displayCount = $derived(expanded ? headlines.length : Math.min(headlines.length, 4));

	$effect(() => {
		if (loaded || validIds.length === 0) return;
		loaded = true;

		if (isTauri()) {
			safeCall(() => getStoryHeadlines(validIds), []).then(h => { headlines = h; });
		}
	});

	function navigateToStory(id: number) {
		// Recorded before the navigation. +page.svelte now loads any cited story by id,
		// so a citation to an older story opens rather than landing on a blank page.
		trackCitationClick('/ask', id);
		expandedStoryId.set(id);
		goto('/');
	}

	function truncate(s: string, max: number): string {
		return s.length > max ? s.slice(0, max) + '...' : s;
	}
</script>

{#if validIds.length > 0}
	<div class="mt-2 pt-2 border-t border-border/50">
		<p class="text-[10px] uppercase tracking-wider text-text-muted mb-1.5">
			{showingCited ? 'Sources' : 'Retrieved'} ({validIds.length})
		</p>
		<div class="space-y-1">
			{#if headlines.length > 0}
				{#each headlines.slice(0, displayCount) as story}
					{@const color = SECTORS[story.sector as SectorId]?.color ?? 'var(--color-text-muted)'}
					<button
						class="block w-full text-left text-[11px] px-2 py-1.5 rounded bg-bg-card border border-border/50
							text-text-secondary hover:bg-bg-card-hover hover:text-text transition-colors cursor-pointer
							flex items-center gap-2"
						onclick={() => navigateToStory(story.id)}
					>
						<span class="w-1.5 h-1.5 rounded-full shrink-0" style="background: {color}"></span>
						<span class="flex-1 truncate">{truncate(story.headline, 70)}</span>
						<span class="text-[9px] text-text-muted shrink-0">{relativeDate(story.date)}</span>
					</button>
				{/each}
				{#if headlines.length > 4 && !expanded}
					<button
						class="text-[10px] text-ai hover:underline"
						onclick={() => expanded = true}
					>
						+{headlines.length - 4} more sources
					</button>
				{/if}
			{:else}
				<!-- Fallback while loading or if fetch fails -->
				{#each validIds.slice(0, 4) as id}
					<button
						class="block w-full text-left text-[11px] px-2 py-1 rounded bg-bg-card border border-border/50
							text-text-secondary hover:bg-bg-card-hover hover:text-text transition-colors cursor-pointer"
						onclick={() => navigateToStory(id)}
					>
						Story #{id}
					</button>
				{/each}
				{#if validIds.length > 4}
					<span class="text-[10px] text-text-muted">+{validIds.length - 4} more</span>
				{/if}
			{/if}
		</div>
	</div>
{/if}
