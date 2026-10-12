<script lang="ts">
	let {
		value = $bindable(),
		label,
		id,
		suffix = '',
		step = 1,
		error = undefined,
		hint = undefined
	}: { value: number | null; label: string; id: string; suffix?: string; step?: number | string; error?: string; hint?: string } = $props();
</script>

<div class="min-w-0">
	<label for={id} class="block text-xs text-text-secondary mb-1">{label}</label>
	<div class="relative">
		<input
			{id}
			type="number"
			{step}
			bind:value
			aria-invalid={error ? 'true' : undefined}
			aria-describedby={error ? `${id}-err` : undefined}
			class="w-full bg-bg border rounded-lg px-2.5 py-1.5 text-sm font-mono tabular-nums text-text focus-visible:outline-2 focus-visible:outline-ai {suffix ? 'pr-7' : ''} {error ? 'border-rose-400/60' : 'border-border'}"
		/>
		{#if suffix}<span class="absolute right-2.5 top-1/2 -translate-y-1/2 text-xs text-text-muted pointer-events-none">{suffix}</span>{/if}
	</div>
	{#if error}<p id="{id}-err" class="text-[11px] text-rose-400 mt-1">{error}</p>
	{:else if hint}<p class="text-[11px] text-text-muted mt-1">{hint}</p>{/if}
</div>
