<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import RelevanceBadge from './RelevanceBadge.svelte';
	import type { Story } from '$lib/tauri/types';

	let { story, onExpand, focused = false }: { story: Story; onExpand: (s: Story) => void; focused?: boolean } = $props();

	let sector = $derived(SECTORS[story.sector as SectorId]);

	// Extract first sentence of why_it_matters
	let whySnippet = $derived(
		story.why_it_matters.split(/[.!?]\s/)[0] + '.'
	);
</script>

<button
	class="w-full text-left bg-bg-card border rounded-xl p-5 hover:bg-bg-card-hover
		transition-all duration-200 cursor-pointer group {focused ? 'border-ai ring-1 ring-ai/50' : 'border-border'}"
	style="border-left: 3px solid {sector?.color ?? 'var(--color-border)'}"
	data-focused={focused}
	data-url={story.original_url}
	onclick={() => onExpand(story)}
>
	<!-- Header: sector + badges -->
	<div class="flex items-center justify-between mb-3">
		<div class="flex items-center gap-2">
			<span
				class="text-[10px] font-medium uppercase tracking-wider px-2 py-0.5 rounded"
				style="color: {sector?.color}; background: {sector?.dimColor}"
			>
				{sector?.name ?? story.sector}
			</span>
			{#if story.summary_depth === 'deep'}
				<span class="text-[10px] font-medium uppercase tracking-wider px-2 py-0.5 rounded-full bg-ai/15 text-ai border border-ai/25">
					Deep
				</span>
			{/if}
		</div>
		{#if story.relevance_score}
			<RelevanceBadge score={story.relevance_score} />
		{/if}
	</div>

	<!-- Headline -->
	<h3 class="text-base font-semibold text-text mb-2 group-hover:text-ai transition-colors leading-snug">
		{story.headline}
	</h3>

	<!-- Summary excerpt -->
	<p class="text-sm text-text-secondary leading-relaxed mb-2 line-clamp-3">
		{story.summary}
	</p>

	<!-- Why it matters snippet -->
	<p class="text-xs text-text-muted italic line-clamp-1 mb-3">
		{whySnippet}
	</p>

	<!-- Source + time -->
	<div class="flex items-center gap-2 text-xs text-text-muted">
		<span>{story.source_name}</span>
		{#if story.published_at}
			<span>·</span>
			<span>{new Date(story.published_at).toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' })}</span>
		{/if}
	</div>
</button>
