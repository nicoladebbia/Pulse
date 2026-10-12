<script lang="ts">
	import type { StressScenario } from '$lib/tauri/types';
	import { usd, pct, tone } from './format';

	let { stress }: { stress: StressScenario[] } = $props();

	const rows = $derived(stress.filter((s) => Number.isFinite(s.pnl_dollars)));
	const maxAbs = $derived(Math.max(1, ...rows.map((s) => Math.abs(s.pnl_dollars))));
</script>

<div class="bg-bg-card border border-border rounded-xl p-4" data-testid="stress-test">
	<h2 class="text-xs font-semibold text-text-secondary uppercase tracking-wider mb-3">Stress test</h2>
	{#if rows.length === 0}
		<p class="text-xs text-text-secondary py-4 text-center">No scenarios available.</p>
	{:else}
		<ul class="space-y-2.5">
			{#each rows as s (s.label)}
				{@const w = (Math.abs(s.pnl_dollars) / maxAbs) * 50}
				<li>
					<div class="flex items-baseline justify-between text-xs mb-1">
						<span class="text-text">{s.label}</span>
						<span class="font-mono tabular-nums {tone(s.pnl_dollars)}">
							{usd(s.pnl_dollars, true)} <span class="text-text-secondary">({pct(s.pnl_pct, 1, true)})</span>
						</span>
					</div>
					<div class="relative h-2.5 rounded-sm bg-bg" aria-hidden="true">
						<div class="absolute inset-y-[-2px] left-1/2 w-px bg-border"></div>
						<div class="absolute inset-y-0 rounded-sm {s.pnl_dollars < 0 ? 'bg-rose-400/70' : 'bg-emerald-400/70'}"
							style="width:{w}%; {s.pnl_dollars < 0 ? `right:50%` : `left:50%`}"></div>
					</div>
				</li>
			{/each}
		</ul>
		<p class="text-[11px] text-text-secondary mt-3">Assumes each holding moves with the S&amp;P 500 by its beta (1.0 where unknown).</p>
	{/if}
</div>
