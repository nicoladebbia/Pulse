<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import RelevanceBadge from './RelevanceBadge.svelte';
	import type { Story } from '$lib/tauri/types';

	let { story, onClose }: { story: Story; onClose: () => void } = $props();

	let sector = $derived(SECTORS[story.sector as SectorId]);

	// Inline AI conversation state
	interface AiMessage {
		question: string;
		title: string;
		summary: string;
		key_points: string[];
		implications: string;
		watch_next: string;
		sources: { headline: string; sector: string; date: string }[];
	}

	let aiMessages = $state<AiMessage[]>([]);
	let isAsking = $state(false);
	let followUpQuery = $state('');
	let conversationEl: HTMLElement;

	function openSource() {
		const ipc = (window as any).__TAURI_INTERNALS__;
		if (ipc) {
			ipc.invoke('plugin:shell|open', { path: story.original_url });
		} else {
			window.open(story.original_url, '_blank');
		}
	}

	async function askQuestion(question: string) {
		if (isAsking || !question.trim()) return;
		isAsking = true;

		try {
			const ipc = (window as any).__TAURI_INTERNALS__;
			if (!ipc) return;

			const result = await ipc.invoke('ask_pulse', { question });

			aiMessages = [...aiMessages, {
				question,
				title: result.title ?? 'Analysis',
				summary: result.summary ?? '',
				key_points: result.key_points ?? [],
				implications: result.implications ?? '',
				watch_next: result.watch_next ?? '',
				sources: result.source_stories ?? [],
			}];

			followUpQuery = '';

			// Scroll to the new response
			requestAnimationFrame(() => {
				conversationEl?.scrollIntoView({ behavior: 'smooth', block: 'end' });
			});
		} catch (e: any) {
			aiMessages = [...aiMessages, {
				question,
				title: 'Error',
				summary: String(e?.message ?? e),
				key_points: [],
				implications: '',
				watch_next: '',
				sources: [],
			}];
		} finally {
			isAsking = false;
		}
	}

	function askAboutStory() {
		askQuestion(`Tell me more about: ${story.headline}. Give me the full picture — background, key players, implications, and what I should keep an eye on.`);
	}

	function deepDiveWatchNext() {
		askQuestion(`Regarding "${story.headline}" — the Watch Next says: "${story.what_to_watch}". Expand on this — what should I expect, when, and how might it affect me as an AI builder?`);
	}

	function handleFollowUp(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey) {
			event.preventDefault();
			// Add story context to follow-up
			askQuestion(`About "${story.headline}": ${followUpQuery}`);
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

	<!-- Watch Next — clickable deep dive -->
	<div class="mb-6 bg-bg-card border border-border rounded-lg p-4 group">
		<div class="flex items-center justify-between mb-2">
			<h3 class="text-xs font-semibold uppercase tracking-wider text-text-muted">Watch Next</h3>
			<button
				class="text-xs text-ai opacity-0 group-hover:opacity-100 transition-opacity hover:underline disabled:opacity-30"
				onclick={deepDiveWatchNext}
				disabled={isAsking}
			>
				Deep dive →
			</button>
		</div>
		<p class="text-sm text-text leading-relaxed">
			{story.what_to_watch}
		</p>
	</div>

	<!-- Source + Ask More row -->
	<div class="flex items-center justify-between pb-6 border-b border-border">
		<button
			class="text-sm text-ai hover:underline transition-colors"
			onclick={openSource}
		>
			Read original source →
		</button>

		{#if aiMessages.length === 0}
			<button
				class="flex items-center gap-2 text-sm bg-ai/10 text-ai px-4 py-2 rounded-lg
					hover:bg-ai/20 transition-colors disabled:opacity-30"
				onclick={askAboutStory}
				disabled={isAsking}
			>
				{#if isAsking}
					<div class="w-3 h-3 border-2 border-ai border-t-transparent rounded-full animate-spin"></div>
					Thinking...
				{:else}
					◎ Ask more about this
				{/if}
			</button>
		{/if}
	</div>

	<!-- Inline AI Conversation -->
	{#if aiMessages.length > 0 || isAsking}
		<div class="pt-6 space-y-6" bind:this={conversationEl}>
			{#each aiMessages as msg, i}
				<!-- Question -->
				<div class="flex justify-end">
					<div class="bg-ai/10 text-ai text-sm px-4 py-2 rounded-2xl rounded-br-sm max-w-[80%]">
						{msg.question.replace(`About "${story.headline}": `, '')}
					</div>
				</div>

				<!-- Structured answer card -->
				<div class="bg-bg-card border border-border rounded-xl overflow-hidden" style="border-left: 3px solid var(--color-ai)">
					<!-- Title -->
					<div class="px-5 pt-5 pb-3">
						<div class="flex items-center gap-2 mb-2">
							<div class="w-2 h-2 rounded-full bg-ai"></div>
							<span class="text-[10px] font-medium text-text-muted uppercase tracking-wider">Pulse Analysis</span>
						</div>
						<h3 class="text-base font-semibold text-text">{msg.title}</h3>
					</div>

					<!-- Summary -->
					{#if msg.summary}
						<div class="px-5 pb-4">
							<p class="text-sm text-text-secondary leading-relaxed">{msg.summary}</p>
						</div>
					{/if}

					<!-- Key Points -->
					{#if msg.key_points.length > 0}
						<div class="px-5 pb-4">
							<h4 class="text-xs font-semibold uppercase tracking-wider text-text-muted mb-2">Key Points</h4>
							<ul class="space-y-1.5">
								{#each msg.key_points as point}
									<li class="flex items-start gap-2 text-sm text-text">
										<span class="text-ai mt-0.5 shrink-0">▪</span>
										<span>{point}</span>
									</li>
								{/each}
							</ul>
						</div>
					{/if}

					<!-- Implications -->
					{#if msg.implications}
						<div class="mx-5 mb-4 bg-bg-expanded border border-border-subtle rounded-lg p-3">
							<h4 class="text-xs font-semibold uppercase tracking-wider text-text-muted mb-1.5">Why It Matters to You</h4>
							<p class="text-sm text-text leading-relaxed">{msg.implications}</p>
						</div>
					{/if}

					<!-- Watch Next -->
					{#if msg.watch_next}
						<div class="mx-5 mb-4 bg-bg-expanded border border-border-subtle rounded-lg p-3">
							<h4 class="text-xs font-semibold uppercase tracking-wider text-text-muted mb-1.5">Watch Next</h4>
							<p class="text-sm text-text leading-relaxed">{msg.watch_next}</p>
						</div>
					{/if}

					<!-- Sources -->
					{#if msg.sources.length > 0}
						<div class="px-5 pb-4 pt-2 border-t border-border">
							<p class="text-[10px] uppercase tracking-wider text-text-muted mb-2">Based on</p>
							<div class="flex flex-wrap gap-1.5">
								{#each msg.sources.slice(0, 5) as source}
									{@const srcSector = SECTORS[source.sector as SectorId]}
									<span
										class="text-[10px] px-2 py-0.5 rounded-full border"
										style="color: {srcSector?.color ?? 'var(--color-text-muted)'}; border-color: {srcSector?.dimColor ?? 'var(--color-border)'}; background: {srcSector?.dimColor ?? 'transparent'}"
									>
										{source.headline.length > 45 ? source.headline.slice(0, 45) + '...' : source.headline}
									</span>
								{/each}
							</div>
						</div>
					{/if}
				</div>
			{/each}

			<!-- Loading indicator -->
			{#if isAsking}
				<div class="bg-bg-card border border-border rounded-xl p-5">
					<div class="flex items-center gap-3">
						<div class="flex gap-1">
							<div class="w-2 h-2 bg-ai rounded-full animate-bounce" style="animation-delay: 0ms"></div>
							<div class="w-2 h-2 bg-ai rounded-full animate-bounce" style="animation-delay: 150ms"></div>
							<div class="w-2 h-2 bg-ai rounded-full animate-bounce" style="animation-delay: 300ms"></div>
						</div>
						<span class="text-sm text-text-muted">Analyzing your archive...</span>
					</div>
				</div>
			{/if}

			<!-- Follow-up input -->
			{#if !isAsking && aiMessages.length > 0}
				<div class="relative">
					<input
						type="text"
						bind:value={followUpQuery}
						onkeydown={handleFollowUp}
						placeholder="Ask a follow-up about this story..."
						class="w-full bg-bg-card border border-border rounded-xl px-4 py-3 pr-12 text-text text-sm
							placeholder:text-text-muted focus:outline-none focus:border-ai transition-colors"
					/>
					<button
						class="absolute right-3 top-1/2 -translate-y-1/2 text-text-muted hover:text-ai transition-colors
							disabled:opacity-30"
						disabled={!followUpQuery.trim()}
						onclick={() => askQuestion(`About "${story.headline}": ${followUpQuery}`)}
					>
						→
					</button>
				</div>
			{/if}
		</div>
	{/if}
</div>
