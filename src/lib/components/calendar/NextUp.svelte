<script lang="ts">
	import type { MacroEventView } from '$lib/tauri/types';
	import MacroChip from './MacroChip.svelte';
	import { fmtDay, relDays } from './calendar-utils';

	let { events }: { events: MacroEventView[] } = $props();
</script>

{#if events.length > 0}
	<section class="mb-6" aria-labelledby="next-up-h">
		<h2 id="next-up-h" class="text-sm font-semibold text-text mb-2">Next up</h2>
		<ul class="grid grid-cols-3 gap-3 list-none p-0 m-0">
			{#each events as ev (ev.date + ev.kind)}
				<li class="bg-bg-card border border-border rounded-xl p-3 min-w-0">
					<div class="flex items-center justify-between gap-2 mb-1.5">
						<MacroChip kind={ev.kind} />
						<span class="text-[11px] font-mono tabular-nums {ev.days_until <= 1 ? 'text-amber-400' : 'text-text-secondary'}">{relDays(ev.days_until)}</span>
					</div>
					<div class="text-sm font-medium text-text leading-snug">{ev.title}</div>
					<div class="text-xs text-text-secondary mt-1 tabular-nums">{fmtDay(ev.date)} · {ev.time_et} ET</div>
					{#if ev.note}<div class="text-[11px] text-text-muted mt-1 leading-snug">{ev.note}</div>{/if}
				</li>
			{/each}
		</ul>
	</section>
{/if}
