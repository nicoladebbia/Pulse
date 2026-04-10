<script lang="ts">
	import { SECTORS, SECTOR_ORDER } from '$lib/config';
	import type { Briefing } from '$lib/tauri/types';

	let { summary, briefing }: { summary: string | null; briefing: Briefing } = $props();

	const sectorCounts = $derived([
		{ id: 'ai', count: briefing.ai_count },
		{ id: 'miami', count: briefing.miami_count },
		{ id: 'italy', count: briefing.italy_count },
		{ id: 'tech', count: briefing.tech_count },
	]);
</script>

<div class="text-center max-w-2xl mx-auto py-6">
	{#if summary}
		<p class="text-sm leading-relaxed text-text-secondary mb-5">
			{summary}
		</p>
	{/if}

	<!-- Sector count pills -->
	<div class="flex items-center justify-center gap-3">
		{#each sectorCounts as { id, count }}
			{@const sector = SECTORS[id as keyof typeof SECTORS]}
			<span
				class="text-[10px] font-mono px-2.5 py-1 rounded-full flex items-center gap-1.5"
				style="color: {sector.color}; background: {sector.dimColor}"
			>
				<span class="w-1.5 h-1.5 rounded-full" style="background: {sector.color}"></span>
				{sector.name}
				<span class="font-semibold">{count}</span>
			</span>
		{/each}
	</div>
</div>
