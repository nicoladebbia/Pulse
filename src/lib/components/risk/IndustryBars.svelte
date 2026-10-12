<script lang="ts">
	import type { IndustryRisk } from '$lib/tauri/types';
	import { usd, pct, INDUSTRY_CAP_PCT } from './format';

	let { industries }: { industries: IndustryRisk[] } = $props();

	const rows = $derived(
		industries
			.filter((i) => Number.isFinite(i.gross_pct))
			.map((i) => {
				const total = i.long_value + Math.abs(i.short_value);
				const longShare = total > 0 ? i.long_value / total : 1;
				return { ...i, longShare, unknown: i.industry.trim().toLowerCase() === 'unknown' };
			})
			.sort((a, b) => b.gross_pct - a.gross_pct)
	);
	// Scale so the cap marker is always visible.
	const scaleMax = $derived(Math.max(INDUSTRY_CAP_PCT * 1.15, ...rows.map((r) => r.gross_pct)));
	const capLeft = $derived((INDUSTRY_CAP_PCT / scaleMax) * 100);
</script>

<div class="bg-bg-card border border-border rounded-xl p-4" data-testid="industry-bars">
	<div class="flex items-center justify-between mb-3">
		<h2 class="text-xs font-semibold text-text-secondary uppercase tracking-wider">Exposure by industry</h2>
		<span class="text-[11px] text-text-secondary flex items-center gap-3">
			<span class="flex items-center gap-1"><span class="inline-block w-2 h-2 rounded-sm bg-emerald-400/70"></span>long</span>
			<span class="flex items-center gap-1"><span class="inline-block w-2 h-2 rounded-sm bg-rose-400/70"></span>short</span>
			<span class="flex items-center gap-1"><span class="inline-block w-px h-3 bg-amber-400"></span>{INDUSTRY_CAP_PCT}% cap</span>
		</span>
	</div>
	{#if rows.length === 0}
		<p class="text-xs text-text-secondary py-4 text-center">No industries to show.</p>
	{:else}
		<ul class="space-y-2">
			{#each rows as r (r.industry)}
				<li class="grid grid-cols-[8rem_minmax(4rem,1fr)_6.5rem] items-center gap-3 {r.unknown ? 'opacity-60' : ''}">
					<span class="text-xs text-text truncate" title={r.industry}>{r.industry}</span>
					<div class="relative h-3 rounded-sm bg-bg">
						<div class="absolute inset-y-0 left-0 flex rounded-sm overflow-hidden" style="width:{Math.min(100, (r.gross_pct / scaleMax) * 100)}%">
							<div class="bg-emerald-400/70" style="width:{r.longShare * 100}%"></div>
							<div class="bg-rose-400/70" style="width:{(1 - r.longShare) * 100}%"></div>
						</div>
						<div class="absolute inset-y-[-2px] w-px bg-amber-400" style="left:{capLeft}%"></div>
					</div>
					<span class="text-xs font-mono tabular-nums text-right {r.gross_pct > INDUSTRY_CAP_PCT ? 'text-amber-400' : 'text-text-secondary'}">
						{pct(r.gross_pct)} <span class="text-text-muted">· {r.count}</span>
						<span class="sr-only">{r.count} positions, long {usd(r.long_value)}, short {usd(Math.abs(r.short_value))}</span>
					</span>
				</li>
			{/each}
		</ul>
	{/if}
</div>
