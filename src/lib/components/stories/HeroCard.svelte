<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import RelevanceBadge from './RelevanceBadge.svelte';
	import type { Story } from '$lib/tauri/types';

	let { story, onExpand }: { story: Story; onExpand: (s: Story) => void } = $props();

	let sector = $derived(SECTORS[story.sector as SectorId]);
</script>

<button
	class="w-full text-left bg-bg-card border border-border rounded-xl p-6 hover:bg-bg-card-hover
		transition-all duration-200 cursor-pointer group relative overflow-hidden"
	style="border-left: 3px solid {sector?.color ?? 'var(--color-border)'}"
	onclick={() => onExpand(story)}
>
	<!-- Sector badge -->
	<div class="flex items-center justify-between mb-3">
		<span
			class="text-xs font-medium uppercase tracking-wider px-2 py-1 rounded"
			style="color: {sector?.color}; background: {sector?.dimColor}"
		>
			{sector?.name ?? story.sector}
		</span>
		{#if story.relevance_score}
			<RelevanceBadge score={story.relevance_score} />
		{/if}
	</div>

	<!-- Headline -->
	<h2 class="text-xl font-semibold text-text mb-3 group-hover:text-ai transition-colors leading-tight">
		{story.headline}
	</h2>

	<!-- Summary preview -->
	<p class="text-text-secondary text-sm leading-relaxed line-clamp-3">
		{story.summary}
	</p>

	<!-- Source and time -->
	<div class="flex items-center gap-3 mt-4 text-xs text-text-muted">
		<span>{story.source_name}</span>
		{#if story.published_at}
			<span>·</span>
			<span>{new Date(story.published_at).toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' })}</span>
		{/if}
		<span class="ml-auto text-text-muted opacity-0 group-hover:opacity-100 transition-opacity">
			Press Enter to expand ↵
		</span>
	</div>
</button>
