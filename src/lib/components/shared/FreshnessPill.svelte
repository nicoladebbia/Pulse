<script lang="ts">
	// Age-based color pill for "X ago" timestamps.
	// Defaults target pipeline-scale staleness (warn at a day, red at a week).
	let {
		timestamp,
		warnHours = 24,
		staleHours = 168,
		label = '',
		class: klass = '',
	}: {
		timestamp: string | null | undefined;
		warnHours?: number;
		staleHours?: number;
		label?: string;
		class?: string;
	} = $props();

	const ageMs = $derived.by(() => {
		if (!timestamp) return null;
		const iso = timestamp.endsWith('Z') || /[+-]\d{2}:?\d{2}$/.test(timestamp) ? timestamp : `${timestamp}Z`;
		const t = Date.parse(iso);
		return Number.isFinite(t) ? Date.now() - t : null;
	});

	const ageLabel = $derived.by(() => {
		if (ageMs == null) return '—';
		const h = ageMs / 3600_000;
		if (h < 1) return 'just now';
		if (h < 24) return `${Math.floor(h)}h ago`;
		const d = Math.floor(h / 24);
		if (d < 30) return `${d}d ago`;
		return `${Math.floor(d / 30)}mo ago`;
	});

	const tone = $derived.by(() => {
		if (ageMs == null) return 'unknown';
		const h = ageMs / 3600_000;
		if (h >= staleHours) return 'stale';
		if (h >= warnHours) return 'warn';
		return 'fresh';
	});

	const toneClass = $derived(
		tone === 'fresh'
			? 'bg-emerald-500/10 text-emerald-400 border-emerald-500/20'
			: tone === 'warn'
				? 'bg-amber-500/10 text-amber-400 border-amber-500/20'
				: tone === 'stale'
					? 'bg-rose-500/10 text-rose-400 border-rose-500/20'
					: 'bg-bg-card text-text-muted border-border'
	);
</script>

<span class="inline-flex items-center gap-1 px-1.5 py-0.5 rounded border text-[10px] font-mono {toneClass} {klass}" title={timestamp ?? 'No timestamp'}>
	{#if label}<span class="opacity-70">{label}</span>{/if}
	{ageLabel}
	{#if tone === 'stale'}<span aria-hidden="true">·</span><span class="uppercase tracking-wider text-[9px]">stale</span>{/if}
</span>
