<script lang="ts">
	import type { DrawdownInfo } from '$lib/tauri/types';
	import { pct } from './format';

	let { drawdown }: { drawdown: DrawdownInfo } = $props();

	const W = 800;
	const H = 140;
	const PAD_T = 6;
	const PAD_B = 6;

	const chart = $derived.by(() => {
		const pts = drawdown.points.filter((p) => Number.isFinite(p.drawdown_pct));
		if (pts.length < 2) return null;
		const lo = Math.min(-0.5, ...pts.map((p) => p.drawdown_pct));
		const x = (i: number) => (i / (pts.length - 1)) * W;
		// 0% sits at the top, the deepest drawdown at the bottom.
		const y = (v: number) => PAD_T + (Math.min(0, v) / lo) * (H - PAD_T - PAD_B);
		const line = pts.map((p, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(p.drawdown_pct).toFixed(1)}`).join(' ');
		const area = `${line} L${W},${y(0).toFixed(1)} L0,${y(0).toFixed(1)} Z`;
		let minI = 0;
		pts.forEach((p, i) => { if (p.drawdown_pct < pts[minI].drawdown_pct) minI = i; });
		return { pts, line, area, lo, zeroY: y(0), minX: x(minI), minY: y(pts[minI].drawdown_pct), minV: pts[minI].drawdown_pct };
	});
</script>

<div class="bg-bg-card border border-border rounded-xl p-4 mb-5" data-testid="drawdown-chart">
	<div class="flex items-center justify-between mb-3">
		<h2 class="text-xs font-semibold text-text-secondary uppercase tracking-wider">Drawdown from peak</h2>
		{#if drawdown.peak_date}
			<span class="text-[11px] text-text-secondary">Peak {drawdown.peak_date}</span>
		{/if}
	</div>
	{#if !chart}
		<p class="text-xs text-text-secondary py-6 text-center">Not enough history yet. The chart appears once the account has two days of equity.</p>
	{:else}
		<svg viewBox="0 0 {W} {H}" class="w-full h-36" preserveAspectRatio="none" role="img"
			aria-label="Drawdown from {chart.pts[0].date} to {chart.pts[chart.pts.length - 1].date}; worst {pct(chart.minV)}">
			<line x1="0" x2={W} y1={chart.zeroY} y2={chart.zeroY} stroke="var(--color-border)" stroke-width="1" vector-effect="non-scaling-stroke" />
			<path d={chart.area} fill="var(--color-rose-400, #fb7185)" fill-opacity="0.12" />
			<path d={chart.line} fill="none" stroke="var(--color-rose-400, #fb7185)" stroke-width="1.5" stroke-linejoin="round" vector-effect="non-scaling-stroke" />
			<circle cx={chart.minX} cy={chart.minY} r="3" fill="var(--color-rose-400, #fb7185)" vector-effect="non-scaling-stroke" />
		</svg>
		<div class="flex justify-between text-[11px] text-text-secondary font-mono tabular-nums mt-1">
			<span>{chart.pts[0].date}</span>
			<span>Worst {pct(chart.minV)}</span>
			<span>{chart.pts[chart.pts.length - 1].date}</span>
		</div>
	{/if}
</div>
