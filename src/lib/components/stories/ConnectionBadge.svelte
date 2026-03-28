<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';

	let { sector, text }: { sector: string; text: string } = $props();
	let sectorConfig = $derived(SECTORS[sector as SectorId]);
</script>

<span
	class="inline-flex items-center gap-1.5 text-xs px-2 py-1 rounded-full border cursor-pointer
		hover:opacity-80 transition-opacity"
	style="color: {sectorConfig?.color}; border-color: {sectorConfig?.dimColor}; background: {sectorConfig?.dimColor}"
	title={text}
>
	<span class="w-1.5 h-1.5 rounded-full" style="background: {sectorConfig?.color}"></span>
	Connected
</span>
