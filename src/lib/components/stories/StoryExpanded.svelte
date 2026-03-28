<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import RelevanceBadge from './RelevanceBadge.svelte';
	import type { Story } from '$lib/tauri/types';

	let { story, onClose }: { story: Story; onClose: () => void } = $props();

	let sector = $derived(SECTORS[story.sector as SectorId]);

	function openSource() {
		if (window.__TAURI_INTERNALS__) {
			import('@tauri-apps/plugin-shell').then(({ open }) => open(story.original_url));
		} else {
			window.open(story.original_url, '_blank');
		}
	}
</script>

<div class="max-w-3xl mx-auto py-2">
	<!-- Back button -->
	<button
		class="flex items-center gap-2 text-sm text-text-muted hover:text-text transition-colors mb-6"
		onclick={onClose}
	>
		← Back to briefing <span class="text-xs opacity-60">(Esc)</span>
	</button>

	<!-- Sector badge -->
	<div class="flex items-center justify-between mb-4">
		<span
			class="text-xs font-medium uppercase tracking-wider px-2 py-1 rounded"
			style="color: {sector?.color}; background: {sector?.dimColor}"
		>
			{sector?.name ?? story.sector}
		</span>
		{#if story.relevance_score}
			<RelevanceBadge score={story.relevance_score} />
		{/if}
	</div>

	<!-- Headline -->
	<h1 class="text-2xl font-bold text-text leading-tight mb-4">
		{story.headline}
	</h1>

	<!-- Summary -->
	<p class="text-text-secondary text-base leading-relaxed mb-6">
		{story.summary}
	</p>

	<!-- Key Facts -->
	<div class="mb-6">
		<h3 class="text-xs font-semibold uppercase tracking-wider text-text-muted mb-3">Key Facts</h3>
		<ul class="space-y-2">
			{#each story.key_facts as fact}
				<li class="flex items-start gap-2 text-sm text-text">
					<span class="text-ai mt-0.5 shrink-0">▪</span>
					<span>{fact}</span>
				</li>
			{/each}
		</ul>
	</div>

	<!-- Why It Matters -->
	<div class="mb-6 bg-bg-card border border-border rounded-lg p-4">
		<h3 class="text-xs font-semibold uppercase tracking-wider text-text-muted mb-2">Why It Matters</h3>
		<p class="text-sm text-text leading-relaxed">
			{story.why_it_matters}
		</p>
	</div>

	<!-- What to Watch -->
	<div class="mb-6 bg-bg-card border border-border rounded-lg p-4">
		<h3 class="text-xs font-semibold uppercase tracking-wider text-text-muted mb-2">Watch Next</h3>
		<p class="text-sm text-text leading-relaxed">
			{story.what_to_watch}
		</p>
	</div>

	<!-- Sources -->
	<div class="pt-4 border-t border-border">
		<button
			class="text-sm text-ai hover:underline transition-colors"
			onclick={openSource}
		>
			{story.source_name} →
		</button>
	</div>
</div>
