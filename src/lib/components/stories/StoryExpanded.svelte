<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import { goto } from '$app/navigation';
	import RelevanceBadge from './RelevanceBadge.svelte';
	import type { Story } from '$lib/tauri/types';

	let { story, onClose }: { story: Story; onClose: () => void } = $props();

	let sector = $derived(SECTORS[story.sector as SectorId]);

	function openSource() {
		const ipc = (window as any).__TAURI_INTERNALS__;
		if (ipc) {
			ipc.invoke('plugin:shell|open', { path: story.original_url });
		} else {
			window.open(story.original_url, '_blank');
		}
	}

	function askAboutStory() {
		// Store the question in sessionStorage so Ask Pulse can pick it up
		const question = `Tell me more about: ${story.headline}. Give me the full picture — background, key players, implications, and what I should keep an eye on.`;
		sessionStorage.setItem('pulse_ask_prefill', question);
		goto('/ask');
	}

	function deepDiveWatchNext() {
		const question = `Regarding "${story.headline}" — the Watch Next says: "${story.what_to_watch}". Can you expand on this? What should I expect, when, and how might it affect me as an AI builder in Miami?`;
		sessionStorage.setItem('pulse_ask_prefill', question);
		goto('/ask');
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

	<!-- What to Watch — now clickable -->
	<div class="mb-6 bg-bg-card border border-border rounded-lg p-4 group">
		<div class="flex items-center justify-between mb-2">
			<h3 class="text-xs font-semibold uppercase tracking-wider text-text-muted">Watch Next</h3>
			<button
				class="text-xs text-ai opacity-0 group-hover:opacity-100 transition-opacity hover:underline"
				onclick={deepDiveWatchNext}
			>
				Deep dive →
			</button>
		</div>
		<p class="text-sm text-text leading-relaxed">
			{story.what_to_watch}
		</p>
	</div>

	<!-- Actions -->
	<div class="pt-4 border-t border-border flex items-center justify-between">
		<button
			class="text-sm text-ai hover:underline transition-colors"
			onclick={openSource}
		>
			Read original source →
		</button>

		<button
			class="flex items-center gap-2 text-sm bg-ai/10 text-ai px-4 py-2 rounded-lg
				hover:bg-ai/20 transition-colors"
			onclick={askAboutStory}
		>
			◎ Ask more about this
		</button>
	</div>
</div>
