<script lang="ts">
	// The bot against simply holding the S&P 500 (SPY) — account, closed and
	// open trades. Same dollars, same days.
	import type { Benchmark } from '$lib/tauri/types';

	let { benchmark, error = null }: { benchmark: Benchmark | null; error?: string | null } = $props();

	function money(v: number): string {
		const s = Math.abs(v).toLocaleString('en-US', { style: 'currency', currency: 'USD', maximumFractionDigits: 0 });
		return `${v >= 0 ? '+' : '−'}${s}`;
	}
	function pct(v: number | null | undefined): string {
		if (v == null) return '—';
		return `${v >= 0 ? '+' : ''}${v.toFixed(1)}%`;
	}
	const tone = (v: number) => (v >= 0 ? 'text-emerald-400' : 'text-rose-400');

	const accountEdge = $derived(
		benchmark?.account_return_pct != null && benchmark?.spy_return_pct != null
			? benchmark.account_return_pct - benchmark.spy_return_pct
			: null
	);
	const closedEdge = $derived(benchmark ? benchmark.closed_pnl - benchmark.closed_spy_pnl : 0);
	const openEdge = $derived(benchmark ? benchmark.open_pnl - benchmark.open_spy_pnl : 0);

	// Sparkline: account vs the same money in SPY.
	const W = 280;
	const H = 56;
	const paths = $derived.by(() => {
		const pts = benchmark?.equity_curve ?? [];
		if (pts.length < 2) return null;
		const vals = pts.flatMap((p) => [p.equity, p.spy_equity]);
		const lo = Math.min(...vals);
		const hi = Math.max(...vals);
		const span = hi - lo || 1;
		const x = (i: number) => (i / (pts.length - 1)) * W;
		const y = (v: number) => H - 2 - ((v - lo) / span) * (H - 4);
		const line = (key: 'equity' | 'spy_equity') =>
			pts.map((p, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(p[key]).toFixed(1)}`).join(' ');
		return { bot: line('equity'), spy: line('spy_equity') };
	});
</script>

<div class="bg-bg-card border border-border rounded-xl p-4 mb-5" data-testid="benchmark-card">
	<div class="flex items-center justify-between mb-3">
		<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider">vs the S&amp;P 500</h2>
		{#if benchmark?.since}
			<span class="text-[10px] text-text-muted">since {benchmark.since}</span>
		{/if}
	</div>

	{#if error}
		<p class="text-xs text-rose-400">Couldn't load the S&amp;P comparison: {error}</p>
	{:else if !benchmark}
		<p class="text-xs text-text-muted">Loading the S&amp;P comparison…</p>
	{:else}
		<div class="grid grid-cols-3 gap-3">
			<div>
				<div class="text-[10px] text-text-muted uppercase tracking-wider">Account</div>
				<div class="text-lg font-mono font-bold {tone(accountEdge ?? 0)}">
					{accountEdge == null ? '—' : `${accountEdge >= 0 ? '+' : ''}${accountEdge.toFixed(1)} pts`}
				</div>
				<div class="text-[11px] text-text-muted">
					You {pct(benchmark.account_return_pct)} · S&amp;P {pct(benchmark.spy_return_pct)}
				</div>
			</div>
			<div>
				<div class="text-[10px] text-text-muted uppercase tracking-wider">Closed trades</div>
				<div class="text-lg font-mono font-bold {tone(closedEdge)}">{money(closedEdge)}</div>
				<div class="text-[11px] text-text-muted">
					Bot {money(benchmark.closed_pnl)} · S&amp;P {money(benchmark.closed_spy_pnl)} ·
					beat it {benchmark.closed_beat_spy}/{benchmark.closed_count}
				</div>
			</div>
			<div>
				<div class="text-[10px] text-text-muted uppercase tracking-wider">Open trades</div>
				<div class="text-lg font-mono font-bold {tone(openEdge)}">{money(openEdge)}</div>
				<div class="text-[11px] text-text-muted">
					Bot {money(benchmark.open_pnl)} · S&amp;P {money(benchmark.open_spy_pnl)}
				</div>
			</div>
		</div>
		{#if paths}
			<svg viewBox="0 0 {W} {H}" class="w-full h-14 mt-3" preserveAspectRatio="none" aria-label="Account value against the same money in the S&P 500">
				<path d={paths.spy} fill="none" stroke="currentColor" class="text-zinc-500" stroke-width="1.5" stroke-dasharray="3 3" vector-effect="non-scaling-stroke" />
				<path d={paths.bot} fill="none" stroke="currentColor" class={tone(accountEdge ?? 0)} stroke-width="1.5" vector-effect="non-scaling-stroke" />
			</svg>
			<div class="flex gap-4 text-[10px] text-text-muted mt-1">
				<span><span class="inline-block w-3 border-t-2 {accountEdge != null && accountEdge < 0 ? 'border-rose-400' : 'border-emerald-400'} align-middle mr-1"></span>Your account</span>
				<span><span class="inline-block w-3 border-t-2 border-dashed border-zinc-500 align-middle mr-1"></span>Same money in the S&amp;P 500</span>
			</div>
		{/if}
		<p class="text-[10px] text-text-muted mt-2">Each trade is compared with putting the same dollars in SPY for the same days. Older trades use SPY's daily open or close, so they are approximate.</p>
	{/if}
</div>
