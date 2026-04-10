<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import type { Story, StoryTrendBadge } from '$lib/tauri/types';

	let { story, onExpand, focused = false, trendBadge = undefined }: {
		story: Story; onExpand: (s: Story) => void; focused?: boolean; trendBadge?: StoryTrendBadge;
	} = $props();

	let sector = $derived(SECTORS[story.sector as SectorId]);

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
	<!-- Sector pill -->
	<div class="flex items-center gap-2 mb-2">
		<span
			class="text-[10px] font-medium uppercase tracking-wider px-2 py-0.5 rounded"
			style="color: {sector?.color}; background: {sector?.dimColor}"
		>
			{sector?.name ?? story.sector}
		</span>
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

	<!-- Footer: source + meta badges -->
	<div class="flex items-center justify-between">
		<div class="flex items-center gap-2 text-xs text-text-muted">
			<span>{story.source_name}</span>
			{#if story.published_at}
				<span>·</span>
				<span>{new Date(story.published_at).toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' })}</span>
			{/if}
		</div>
		<div class="flex items-center gap-1.5">
			{#if story.summary_depth === 'deep'}
				<span class="text-[9px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded-full bg-ai/15 text-ai">
					Deep
				</span>
			{/if}
			{#if trendBadge}
				<span class="text-[9px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded-full
					{trendBadge.trajectory === 'dominant' ? 'bg-ai/15 text-ai' : 'bg-amber-500/15 text-amber-400'}"
					title="{trendBadge.entity}">
					↗ Trending
				</span>
			{/if}
			{#if story.relevance_score && story.relevance_score >= 7}
				<span class="text-[9px] font-medium px-1.5 py-0.5 rounded-full bg-emerald-500/15 text-emerald-400">
					{story.relevance_score}/10
				</span>
			{/if}
		</div>
	</div>
</button>
