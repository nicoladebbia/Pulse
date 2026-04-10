<script lang="ts">
	import { goto } from '$app/navigation';
	import { expandedStoryId } from '$lib/stores/briefing';
	import { getStoryHeadlines } from '$lib/tauri/commands';
	import { isTauri } from '$lib/tauri/mock';
	import { SECTORS, type SectorId } from '$lib/config';
	import type { StoryHeadline } from '$lib/tauri/types';

	let { storyIds }: { storyIds: number[] } = $props();

	let headlines = $state<StoryHeadline[]>([]);
	let loaded = $state(false);
	let expanded = $state(false);

	const validIds = $derived(storyIds.filter(id => id > 0));
	const displayCount = $derived(expanded ? headlines.length : Math.min(headlines.length, 4));

	$effect(() => {
		if (loaded || validIds.length === 0) return;
		loaded = true;

		if (isTauri()) {
			getStoryHeadlines(validIds).then(h => { headlines = h; }).catch(() => {});
		}
	});

	function navigateToStory(id: number) {
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
			Sources ({validIds.length})
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
						<span class="text-[9px] text-text-muted shrink-0">{story.date}</span>
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
