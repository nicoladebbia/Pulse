<script lang="ts">
	import type { PositionRisk } from '$lib/tauri/types';
	import { usd, pct, tone } from './format';

	let { positions }: { positions: PositionRisk[] } = $props();

	type Key = 'symbol' | 'side' | 'industry' | 'market_value' | 'pct_of_equity' | 'unrealized_pl' | 'stop_price' | 'stop_loss_dollars' | 'beta';
	const cols: { key: Key; label: string; num: boolean }[] = [
		{ key: 'symbol', label: 'Symbol', num: false },
		{ key: 'side', label: 'Side', num: false },
		{ key: 'industry', label: 'Industry', num: false },
		{ key: 'market_value', label: 'Value', num: true },
		{ key: 'pct_of_equity', label: '% of equity', num: true },
		{ key: 'unrealized_pl', label: 'P&L', num: true },
		{ key: 'stop_price', label: 'Stop', num: true },
		{ key: 'stop_loss_dollars', label: 'Lost at stop', num: true },
		{ key: 'beta', label: 'Beta', num: true }
	];

	let sortKey = $state<Key>('pct_of_equity');
	let dir = $state<1 | -1>(-1);

	function setSort(k: Key) {
		if (sortKey === k) dir = dir === 1 ? -1 : 1;
		else { sortKey = k; dir = cols.find((c) => c.key === k)?.num ? -1 : 1; }
	}

	function val(p: PositionRisk, k: Key): string | number | null {
		const v = k === 'market_value' ? Math.abs(p.market_value) : p[k];
		if (typeof v === 'number' && !Number.isFinite(v)) return null;
		return v;
	}

	// Missing/NaN values always sort last, whichever the direction.
	const sorted = $derived(
		[...positions].sort((a, b) => {
			const x = val(a, sortKey);
			const y = val(b, sortKey);
			if (x == null && y == null) return a.symbol.localeCompare(b.symbol);
			if (x == null) return 1;
			if (y == null) return -1;
			const c = typeof x === 'string' || typeof y === 'string' ? String(x).localeCompare(String(y)) : x - y;
			return c === 0 ? a.symbol.localeCompare(b.symbol) : c * dir;
		})
	);
</script>

<div class="bg-bg-card border border-border rounded-xl overflow-hidden" data-testid="positions-table">
	<div class="px-4 pt-4 pb-2">
		<h2 class="text-xs font-semibold text-text-secondary uppercase tracking-wider">Positions</h2>
	</div>
	<div class="overflow-x-auto">
		<table class="w-full text-xs tabular-nums">
			<thead>
				<tr class="border-b border-border">
					{#each cols as c (c.key)}
						<th scope="col" class="px-3 py-2 font-medium {c.num ? 'text-right' : 'text-left'} first:pl-4 last:pr-4"
							aria-sort={sortKey === c.key ? (dir === 1 ? 'ascending' : 'descending') : 'none'}>
							<button type="button" onclick={() => setSort(c.key)}
								class="inline-flex items-center gap-1 text-text-secondary hover:text-text focus-visible:outline-2 focus-visible:outline-ai rounded-sm {sortKey === c.key ? 'text-text' : ''}">
								{c.label}
								<span aria-hidden="true" class="w-2 text-[9px]">{sortKey === c.key ? (dir === 1 ? '▲' : '▼') : ''}</span>
							</button>
						</th>
					{/each}
				</tr>
			</thead>
			<tbody>
				{#each sorted as p (p.symbol + p.side)}
					<tr class="border-b border-border-subtle last:border-0 hover:bg-bg-card-hover">
						<td class="pl-4 pr-3 py-2 font-mono font-semibold text-text">{p.symbol}</td>
						<td class="px-3 py-2">
							{#if p.side === 'short'}
								<span class="rounded px-1.5 py-0.5 text-[10px] font-semibold bg-rose-400/15 text-rose-400">SHORT</span>
							{:else}
								<span class="rounded px-1.5 py-0.5 text-[10px] font-medium bg-emerald-400/10 text-emerald-400">Long</span>
							{/if}
						</td>
						<td class="px-3 py-2 text-text-secondary {p.industry.toLowerCase() === 'unknown' ? 'opacity-70' : ''}">{p.industry}</td>
						<td class="px-3 py-2 text-right font-mono text-text">{usd(Math.abs(p.market_value))}</td>
						<td class="px-3 py-2 text-right font-mono text-text">{pct(p.pct_of_equity)}</td>
						<td class="px-3 py-2 text-right font-mono {tone(p.unrealized_pl)}">{usd(p.unrealized_pl, true)}</td>
						<td class="px-3 py-2 text-right font-mono text-text-secondary">
							{p.stop_price != null && Number.isFinite(p.stop_price) ? `$${p.stop_price.toFixed(2)}` : '—'}
						</td>
						<td class="px-3 py-2 text-right font-mono">
							{#if p.stop_loss_dollars == null || !Number.isFinite(p.stop_loss_dollars)}
								<span class="text-amber-400 font-sans">no stop</span>
							{:else}
								<span class="text-rose-400">−{usd(p.stop_loss_dollars)}</span>
							{/if}
						</td>
						<td class="pl-3 pr-4 py-2 text-right font-mono text-text-secondary">
							{p.beta != null && Number.isFinite(p.beta) ? p.beta.toFixed(2) : '—'}
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
</div>
