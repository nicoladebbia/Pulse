<script lang="ts">
	// The checklist for "has the paper bot earned real money?". Fixed thresholds, written down
	// in advance; Pulse never switches to real money by itself.
	import type { GoLiveGate } from '$lib/tauri/types';

	let { gate, error = null }: { gate: GoLiveGate | null; error?: string | null } = $props();

	const badge = $derived(
		gate?.status === 'ready'
			? { text: 'Ready to consider', cls: 'bg-emerald-500/15 text-emerald-400' }
			: gate?.status === 'close'
				? { text: 'Close', cls: 'bg-amber-500/15 text-amber-400' }
				: { text: 'Not yet', cls: 'bg-rose-500/15 text-rose-400' }
	);
</script>

<div class="bg-bg-card border border-border rounded-xl p-4 mb-5" data-testid="go-live-card">
	<div class="flex items-center justify-between mb-3">
		<h2 class="text-xs font-semibold text-text-muted uppercase tracking-wider">Ready for real money?</h2>
		{#if gate}
			<span class="text-[10px] font-semibold px-2 py-0.5 rounded-full {badge.cls}">{badge.text}</span>
		{/if}
	</div>

	{#if error}
		<p class="text-xs text-rose-400">Couldn't load the checklist: {error}</p>
	{:else if !gate}
		<p class="text-xs text-text-muted">Loading the checklist…</p>
	{:else}
		<p class="text-sm text-text mb-3">{gate.headline}</p>
		<ul class="space-y-2">
			{#each gate.criteria as c (c.key)}
				<li class="flex items-start gap-2" title={c.detail}>
					<span class="mt-0.5 text-xs {c.passed ? 'text-emerald-400' : 'text-text-muted'}" aria-hidden="true">
						{c.passed ? '✓' : '○'}
					</span>
					<div class="flex-1 min-w-0">
						<div class="text-xs text-text">{c.label}</div>
						<div class="text-[11px] text-text-muted">{c.detail}</div>
					</div>
					<div class="text-right shrink-0">
						<div class="text-xs font-mono {c.passed ? 'text-emerald-400' : 'text-text'}">{c.current}</div>
						<div class="text-[10px] text-text-muted">need {c.target}</div>
					</div>
				</li>
			{/each}
		</ul>
		<p class="text-[10px] text-text-muted mt-3">
			Paper results are not a promise. Pulse never moves to real money on its own; if every check
			is met, start with an amount you can afford to lose.
		</p>
	{/if}
</div>
