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

	let timeStr = $derived(
		story.published_at
			? new Date(story.published_at).toLocaleTimeString('en-US', { hour: 'numeric', minute: '2-digit' })
			: null
	);
</script>

<button
	class="w-full text-left bg-bg-card border rounded-xl p-4 hover:bg-bg-card-hover
		transition-all duration-200 cursor-pointer group {focused ? 'border-ai ring-1 ring-ai/50' : 'border-border'}"
	data-focused={focused}
	data-url={story.original_url}
	onclick={() => onExpand(story)}
>
	<!-- Top: sector + optional trend dot -->
	<div class="flex items-center gap-2 mb-2">
		<span
			class="text-[10px] font-medium uppercase tracking-wider px-2 py-0.5 rounded"
			style="color: {sector?.color}; background: {sector?.dimColor}"
		>
			{sector?.name ?? story.sector}
		</span>
		{#if trendBadge}
			<span class="w-1.5 h-1.5 rounded-full bg-emerald-400 animate-pulse" title="Trending: {trendBadge.entity}"></span>
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

	<!-- Footer: source · time -->
	<div class="text-[11px] text-text-muted truncate">
		{story.source_name}{#if timeStr} · {timeStr}{/if}
	</div>
</button>
