<script lang="ts">
	import type { BacktestEquityPoint } from '$lib/tauri/types';

	let { curve, spy, baseline }: { curve: BacktestEquityPoint[]; spy: BacktestEquityPoint[]; baseline: number } = $props();

	const W = 800;
	const H = 260;
	const PL = 64;
	const PR = 12;
	const PT = 12;
	const PB = 26;

	const money = (v: number) => v.toLocaleString('en-US', { style: 'currency', currency: 'USD', maximumFractionDigits: 0 });
	const spyByDate = $derived(new Map(spy.map((p) => [p.date, p.value])));
	const hasSpy = $derived(spy.length > 1);

	const scale = $derived.by(() => {
		const vals = [baseline, ...curve.map((p) => p.value), ...(hasSpy ? spy.map((p) => p.value) : [])].filter(Number.isFinite);
		let lo = Math.min(...vals);
		let hi = Math.max(...vals);
		const pad = (hi - lo || hi * 0.02 || 1) * 0.06;
		lo -= pad;
		hi += pad;
		return { lo, hi };
	});
	const n = $derived(curve.length);
	const x = (i: number) => PL + (n > 1 ? (i / (n - 1)) * (W - PL - PR) : 0);
	const y = (v: number) => PT + (1 - (v - scale.lo) / (scale.hi - scale.lo)) * (H - PT - PB);
	const path = (vals: (number | undefined)[]) => {
		let d = '';
		vals.forEach((v, i) => {
			if (v == null || !Number.isFinite(v)) return;
			d += `${d ? 'L' : 'M'}${x(i).toFixed(1)},${y(v).toFixed(1)}`;
		});
		return d;
	};
	const botPath = $derived(path(curve.map((p) => p.value)));
	const spyPath = $derived(hasSpy ? path(curve.map((p) => spyByDate.get(p.date))) : '');
	const areaPath = $derived(botPath ? `${botPath}L${x(n - 1).toFixed(1)},${y(scale.lo).toFixed(1)}L${x(0).toFixed(1)},${y(scale.lo).toFixed(1)}Z` : '');
	const ticks = $derived([0, 1, 2, 3].map((k) => scale.lo + ((scale.hi - scale.lo) * k) / 3));
	const up = $derived(n > 0 && curve[n - 1].value >= baseline);

	let hover = $state<number | null>(null);
	let svgEl = $state<SVGSVGElement | null>(null);

	function onMove(e: PointerEvent) {
		if (!svgEl || n < 2) return;
		const r = svgEl.getBoundingClientRect();
		const px = ((e.clientX - r.left) / r.width) * W;
		hover = Math.max(0, Math.min(n - 1, Math.round(((px - PL) / (W - PL - PR)) * (n - 1))));
	}
	function onKey(e: KeyboardEvent) {
		if (n < 2) return;
		if (e.key === 'ArrowLeft') hover = Math.max(0, (hover ?? n - 1) - 1);
		else if (e.key === 'ArrowRight') hover = Math.min(n - 1, (hover ?? 0) + 1);
		else if (e.key === 'Escape') hover = null;
		else return;
		e.preventDefault();
	}
	const hp = $derived(hover != null ? curve[hover] : null);
	const hSpy = $derived(hp ? spyByDate.get(hp.date) : undefined);
</script>

<div>
	<div class="flex flex-wrap items-center justify-between gap-2 mb-2 min-h-5 text-xs">
		<div class="flex items-center gap-4 text-text-secondary">
			<span class="flex items-center gap-1.5"><span class="w-3 h-0.5 rounded bg-ai"></span>Pulse bot</span>
			{#if hasSpy}<span class="flex items-center gap-1.5"><span class="w-3 border-t-2 border-dashed border-text-muted"></span>S&amp;P 500 (SPY)</span>{/if}
			<span class="flex items-center gap-1.5"><span class="w-3 border-t border-dotted border-text-muted"></span>Starting {money(baseline)}</span>
		</div>
		<div class="font-mono tabular-nums text-text-secondary" aria-live="polite">
			{#if hp}
				<span class="text-text-muted">{hp.date}</span>
				<span class="text-text ml-2">{money(hp.value)}</span>
				{#if hSpy != null}<span class="ml-2 text-text-muted">SPY {money(hSpy)}</span>{/if}
			{/if}
		</div>
	</div>
	<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
	<svg
		bind:this={svgEl}
		viewBox="0 0 {W} {H}"
		class="w-full h-auto block touch-none focus-visible:outline-2 focus-visible:outline-ai rounded"
		role="img"
		tabindex="0"
		aria-label="Equity curve from {curve[0]?.date} to {curve[n - 1]?.date}: {money(curve[0]?.value ?? 0)} to {money(curve[n - 1]?.value ?? 0)}. Use left and right arrow keys to read values."
		onpointermove={onMove}
		onpointerleave={() => (hover = null)}
		onkeydown={onKey}
		onblur={() => (hover = null)}
	>
		<defs>
			<linearGradient id="eq-fill" x1="0" y1="0" x2="0" y2="1">
				<stop offset="0" stop-color="var(--color-ai)" stop-opacity="0.18" />
				<stop offset="1" stop-color="var(--color-ai)" stop-opacity="0" />
			</linearGradient>
		</defs>
		{#each ticks as t (t)}
			<line x1={PL} x2={W - PR} y1={y(t)} y2={y(t)} stroke="var(--color-border-subtle)" />
			<text x={PL - 8} y={y(t) + 3.5} text-anchor="end" class="fill-text-muted" font-size="10" font-family="var(--font-mono)">{money(t)}</text>
		{/each}
		{#if baseline > scale.lo && baseline < scale.hi}
			<line x1={PL} x2={W - PR} y1={y(baseline)} y2={y(baseline)} stroke="var(--color-text-muted)" stroke-dasharray="1 4" stroke-linecap="round" />
		{/if}
		<path d={areaPath} fill="url(#eq-fill)" />
		{#if spyPath}<path d={spyPath} fill="none" stroke="var(--color-text-muted)" stroke-width="1.5" stroke-dasharray="5 4" />{/if}
		<path d={botPath} fill="none" stroke="var(--color-ai)" stroke-width="2" stroke-linejoin="round" />
		{#if n > 0}
			<circle cx={x(n - 1)} cy={y(curve[n - 1].value)} r="3.5" fill={up ? 'var(--color-relevance-high)' : '#fb7185'} />
			<text x={PL} y={H - 8} class="fill-text-muted" font-size="10" font-family="var(--font-mono)">{curve[0].date}</text>
			<text x={W - PR} y={H - 8} text-anchor="end" class="fill-text-muted" font-size="10" font-family="var(--font-mono)">{curve[n - 1].date}</text>
		{/if}
		{#if hp && hover != null}
			<line x1={x(hover)} x2={x(hover)} y1={PT} y2={H - PB} stroke="var(--color-text-muted)" />
			{#if hSpy != null}<circle cx={x(hover)} cy={y(hSpy)} r="3" fill="var(--color-bg-card)" stroke="var(--color-text-secondary)" stroke-width="1.5" />{/if}
			<circle cx={x(hover)} cy={y(hp.value)} r="4" fill="var(--color-ai)" stroke="var(--color-bg-card)" stroke-width="2" />
		{/if}
	</svg>
</div>
