<script lang="ts">
	import type { BacktestTrade } from '$lib/tauri/types';
	import { exitLabel } from './config';

	let { trades }: { trades: BacktestTrade[] } = $props();

	type Key = 'ticker' | 'entry_date' | 'exit_date' | 'holding_days' | 'pnl_pct' | 'exit_reason';
	let sortKey = $state<Key>('entry_date');
	let dir = $state<1 | -1>(-1);
	let showAll = $state(false);

	function setSort(k: Key) {
		if (sortKey === k) dir = dir === 1 ? -1 : 1;
		else {
			sortKey = k;
			dir = k === 'ticker' || k === 'exit_reason' ? 1 : -1;
		}
	}

	const sorted = $derived.by(() => {
		const k = sortKey;
		const out = [...trades];
		out.sort((a, b) => {
			const av = a[k];
			const bv = b[k];
			const aBad = typeof av === 'number' && !Number.isFinite(av);
			const bBad = typeof bv === 'number' && !Number.isFinite(bv);
			if (aBad || bBad) return aBad === bBad ? 0 : aBad ? 1 : -1; // NaN always last
			const c = typeof av === 'number' && typeof bv === 'number' ? av - bv : String(av ?? '').localeCompare(String(bv ?? ''));
			return c * dir;
		});
		return out;
	});
	const shown = $derived(showAll ? sorted : sorted.slice(0, 50));
	const pct = (v: number) => (Number.isFinite(v) ? `${v >= 0 ? '+' : '−'}${Math.abs(v).toFixed(1)}%` : '—');
	const cols: { k: Key; label: string; right?: boolean }[] = [
		{ k: 'ticker', label: 'Ticker' },
		{ k: 'entry_date', label: 'Entry' },
		{ k: 'exit_date', label: 'Exit' },
		{ k: 'holding_days', label: 'Held', right: true },
		{ k: 'pnl_pct', label: 'P&L', right: true },
		{ k: 'exit_reason', label: 'Exit reason' }
	];
</script>

<div class="overflow-x-auto">
	<table class="w-full text-sm">
		<thead>
			<tr class="border-b border-border">
				{#each cols as c (c.k)}
					<th scope="col" class="py-2 px-2 first:pl-0 font-normal text-xs {c.right ? 'text-right' : 'text-left'}" aria-sort={sortKey === c.k ? (dir === 1 ? 'ascending' : 'descending') : 'none'}>
						<button type="button" onclick={() => setSort(c.k)} class="text-text-secondary hover:text-text rounded focus-visible:outline-2 focus-visible:outline-ai {sortKey === c.k ? 'text-text' : ''}">
							{c.label}{sortKey === c.k ? (dir === 1 ? ' ↑' : ' ↓') : ''}
						</button>
					</th>
				{/each}
			</tr>
		</thead>
		<tbody>
			{#each shown as t, i (`${t.ticker}-${t.entry_date}-${i}`)}
				<tr class="border-b border-border-subtle">
					<td class="py-1.5 px-2 pl-0"><span class="font-mono font-medium text-text">{t.ticker}</span> <span class="text-xs text-text-muted hidden xl:inline">{t.entity_name}</span></td>
					<td class="py-1.5 px-2 font-mono tabular-nums text-text-secondary">{t.entry_date}</td>
					<td class="py-1.5 px-2 font-mono tabular-nums text-text-secondary">{t.exit_date}</td>
					<td class="py-1.5 px-2 font-mono tabular-nums text-right text-text-secondary">{Number.isFinite(t.holding_days) ? `${t.holding_days}d` : '—'}</td>
					<td class="py-1.5 px-2 font-mono tabular-nums text-right {t.pnl_pct >= 0 ? 'text-emerald-400' : 'text-rose-400'}">{pct(t.pnl_pct)}</td>
					<td class="py-1.5 px-2 text-xs {t.exit_reason === 'data_end' ? 'text-amber-400' : 'text-text-secondary'}">{exitLabel(t.exit_reason)}</td>
				</tr>
			{/each}
		</tbody>
	</table>
</div>
{#if sorted.length > 50}
	<button type="button" onclick={() => (showAll = !showAll)} class="mt-3 text-xs text-ai hover:underline rounded focus-visible:outline-2 focus-visible:outline-ai">
		{showAll ? 'Show first 50' : `Show all ${sorted.length}`}
	</button>
{/if}
