<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import type { BriefingConnection } from '$lib/tauri/types';

	let { connection }: { connection: BriefingConnection } = $props();

	let sectorA = $derived(SECTORS[connection.sector_a as SectorId]);
	let sectorB = $derived(SECTORS[connection.sector_b as SectorId]);
</script>

<div class="bg-bg-card/50 border border-border/50 rounded-lg p-4">
	<div class="flex items-center gap-2 mb-2.5">
		<span class="text-text-muted text-xs">~</span>
		<span
			class="text-[9px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded"
			style="color: {sectorA?.color}; background: {sectorA?.dimColor}"
		>
			{connection.sector_a}
		</span>
		<span class="text-text-muted text-xs">↔</span>
		<span
			class="text-[9px] font-medium uppercase tracking-wider px-1.5 py-0.5 rounded"
			style="color: {sectorB?.color}; background: {sectorB?.dimColor}"
		>
			{connection.sector_b}
		</span>
	</div>
	<p class="text-sm text-text-secondary italic mb-1">
		{connection.connection_text}
	</p>
	<p class="text-sm text-text leading-relaxed">
		{connection.insight_text}
	</p>
</div>
