<script lang="ts">
	// "Why did Pulse buy this?" in plain words: the reason, the size, the exit
	// plan and how it is going. The text comes from what the trade stored when it
	// was opened, so it never invents a catalyst.
	import type { TradeExplanation } from '$lib/tauri/types';

	let { explanation, open = $bindable(false) }: { explanation: TradeExplanation; open?: boolean } = $props();

	const rows = $derived(
		[
			{ label: 'Why Pulse bought it', text: explanation.why },
			{ label: 'How much, and why', text: explanation.size },
			{ label: 'When it will sell', text: explanation.plan },
			{ label: 'How it is going', text: explanation.now }
		].filter((r): r is { label: string; text: string } => !!r.text)
	);
</script>

<div class="mt-2">
	<button
		type="button"
		class="text-[11px] text-blue-400 hover:text-blue-300 transition-colors"
		aria-expanded={open}
		onclick={() => (open = !open)}
	>
		{open ? '▾' : '▸'} {explanation.headline} — why Pulse bought this
	</button>
	{#if open}
		<dl class="mt-1.5 bg-bg border border-border rounded-lg p-3 space-y-2">
			{#each rows as r}
				<div>
					<dt class="text-[10px] uppercase tracking-wider text-text-muted">{r.label}</dt>
					<dd class="text-xs text-text-secondary leading-relaxed">{r.text}</dd>
				</div>
			{/each}
		</dl>
	{/if}
</div>
