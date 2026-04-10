<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import type { Story } from '$lib/tauri/types';

	let { story, onExpand, focused = false }: { story: Story; onExpand: (s: Story) => void; focused?: boolean } = $props();

	let sector = $derived(SECTORS[story.sector as SectorId]);
</script>

<button
	class="w-full flex items-center gap-3 py-2.5 px-3 hover:bg-bg-card-hover rounded-lg
		cursor-pointer transition-colors text-left {focused ? 'bg-ai/5 ring-1 ring-ai/50' : ''}"
	data-focused={focused}
	data-url={story.original_url}
	onclick={() => onExpand(story)}
>
	<!-- Sector pill -->
	<span
		class="text-[9px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded shrink-0"
		style="color: {sector?.color}; background: {sector?.dimColor}"
	>
		{story.sector}
	</span>

	<!-- Deep badge -->
	{#if story.summary_depth === 'deep'}
		<span class="text-[8px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded-full bg-ai/15 text-ai shrink-0">
			Deep
		</span>
	{/if}

	<!-- Headline -->
	<span class="text-sm text-text flex-1 line-clamp-1">
		{story.headline}
	</span>

	<!-- Source -->
	<span class="text-xs text-text-muted shrink-0">
		{story.source_name}
	</span>
</button>
