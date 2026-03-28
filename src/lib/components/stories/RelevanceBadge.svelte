<script lang="ts">
	let { score, size = 'md' }: { score: number; size?: 'sm' | 'md' } = $props();

	let color = $derived(
		score >= 8 ? 'var(--color-relevance-high)' :
		score >= 5 ? 'var(--color-relevance-mid)' :
		'var(--color-relevance-low)'
	);

	let sizeClass = $derived(size === 'sm' ? 'text-[10px] px-1.5 py-0.5' : 'text-xs px-2 py-1');
</script>

<span
	class="font-mono font-medium rounded {sizeClass}"
	style="color: {color}; background: color-mix(in srgb, {color} 15%, transparent)"
>
	{score}/10
</span>
