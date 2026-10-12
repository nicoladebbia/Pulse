<script lang="ts">
	import type { BacktestMonthlyReturn } from '$lib/tauri/types';

	let { months }: { months: BacktestMonthlyReturn[] } = $props();

	const max = $derived(Math.max(1, ...months.map((m) => Math.abs(m.pnl_pct)).filter(Number.isFinite)));
	const fmt = (v: number) => `${v >= 0 ? '+' : '−'}${Math.abs(v).toFixed(1)}%`;
	const label = (m: string) => {
		const [y, mo] = m.split('-');
		return new Date(Number(y), Number(mo) - 1, 1).toLocaleDateString('en-US', { month: 'short' }) + (mo === '01' || m === months[0]?.month ? ` ’${y.slice(2)}` : '');
	};
</script>

<div class="flex items-stretch gap-1.5 overflow-x-auto" role="list" aria-label="Monthly returns">
	{#each months as m (m.month)}
		{@const v = Number.isFinite(m.pnl_pct) ? m.pnl_pct : 0}
		<div role="listitem" class="flex-1 min-w-11 max-w-24 flex flex-col items-center" title="{m.month}: {fmt(v)} on {m.trade_count} trades">
			<span class="text-[11px] font-mono tabular-nums mb-1 {v >= 0 ? 'text-emerald-400' : 'text-rose-400'}">{fmt(v)}</span>
			<div class="relative w-full h-28">
				<div class="absolute inset-x-0 top-1/2 border-t border-border"></div>
				<div
					class="absolute inset-x-1.5 rounded-sm {v >= 0 ? 'bg-emerald-400/70 bottom-1/2 rounded-b-none' : 'bg-rose-400/70 top-1/2 rounded-t-none'}"
					style="height: {Math.max(1, (Math.abs(v) / max) * 50)}%"
				></div>
			</div>
			<span class="text-[11px] text-text-muted mt-1 whitespace-nowrap">{label(m.month)}</span>
		</div>
	{/each}
</div>
