<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import RelevanceBadge from './RelevanceBadge.svelte';
	import type { Story } from '$lib/tauri/types';

	let { story, onExpand }: { story: Story; onExpand: (s: Story) => void } = $props();

	let sector = $derived(SECTORS[story.sector as SectorId]);
</script>

<button
	class="w-full text-left bg-bg-card border border-border rounded-lg p-4 hover:bg-bg-card-hover
		transition-all duration-200 cursor-pointer group"
	style="border-left: 3px solid {sector?.color ?? 'var(--color-border)'}"
	onclick={() => onExpand(story)}
>
	<!-- Header row -->
	<div class="flex items-start justify-between gap-2 mb-2">
		<span
			class="text-[10px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded shrink-0"
			style="color: {sector?.color}; background: {sector?.dimColor}"
		>
			{sector?.name ?? story.sector}
		</span>
		{#if story.relevance_score}
			<RelevanceBadge score={story.relevance_score} size="sm" />
		{/if}
	</div>

	<!-- Headline -->
	<h3 class="text-sm font-medium text-text mb-1.5 line-clamp-2 group-hover:text-ai transition-colors leading-snug">
		{story.headline}
	</h3>

	<!-- Source -->
	<div class="text-xs text-text-muted">
		{story.source_name}
	</div>
</button>
