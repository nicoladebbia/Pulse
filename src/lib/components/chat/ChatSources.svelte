<script lang="ts">
	import { goto } from '$app/navigation';
	import { expandedStoryId, currentBriefing } from '$lib/stores/briefing';

	let { storyIds }: { storyIds: number[] } = $props();

	function getHeadline(id: number): string | null {
		const stories = $currentBriefing?.stories ?? [];
		return stories.find(s => s.id === id)?.headline ?? null;
	}

	function navigateToStory(id: number) {
		expandedStoryId.set(id);
		goto('/');
	}

	function truncate(s: string, max: number): string {
		return s.length > max ? s.slice(0, max) + '...' : s;
	}
</script>

{#if storyIds.length > 0}
	<div class="mt-2 pt-2 border-t border-border/50">
		<p class="text-[10px] uppercase tracking-wider text-text-muted mb-1.5">Sources</p>
		<div class="space-y-1">
			{#each storyIds.filter(id => id > 0).slice(0, 6) as id}
				{@const headline = getHeadline(id)}
				<button
					class="block w-full text-left text-[11px] px-2 py-1 rounded bg-ai/5 text-text-secondary hover:bg-ai/15 hover:text-ai transition-colors cursor-pointer"
					onclick={() => navigateToStory(id)}
				>
					{#if headline}
						{truncate(headline, 80)}
					{:else}
						Story #{id}
					{/if}
				</button>
			{/each}
			{#if storyIds.length > 6}
				<span class="text-[10px] text-text-muted">+{storyIds.length - 6} more</span>
			{/if}
		</div>
	</div>
{/if}
