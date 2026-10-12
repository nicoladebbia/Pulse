<script lang="ts" generics="T extends string">
	let { value = $bindable(), options, label }: { value: T; options: { id: T; label: string }[]; label: string } = $props();
</script>

<div role="radiogroup" aria-label={label} class="inline-flex flex-wrap p-0.5 rounded-lg bg-bg border border-border">
	{#each options as o (o.id)}
		<button
			type="button"
			role="radio"
			aria-checked={value === o.id}
			onclick={() => (value = o.id)}
			class="px-3 py-1.5 text-xs rounded-md transition-colors duration-150 focus-visible:outline-2 focus-visible:outline-ai {value === o.id
				? 'bg-bg-card-hover text-text font-medium shadow-[0_0_0_1px_rgba(255,255,255,0.08)]'
				: 'text-text-secondary hover:text-text'}"
		>
			{o.label}
		</button>
	{/each}
</div>
