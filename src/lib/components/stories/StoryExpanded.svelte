<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import RelevanceBadge from './RelevanceBadge.svelte';
	import type { Story, ChatStreamEvent, StoryEntityContext } from '$lib/tauri/types';
	import { chatSendStream } from '$lib/tauri/commands';
	import { isTauri, simulateChatStream } from '$lib/tauri/mock';

	let { story, onClose }: { story: Story; onClose: () => void } = $props();

	let sector = $derived(SECTORS[story.sector as SectorId]);

	// Inline AI conversation state
	interface AiMessage {
		question: string;
		content: string;
		sources: number[];
		isStreaming: boolean;
	}

	let aiMessages = $state<AiMessage[]>([]);
	let isAsking = $state(false);
	let storyEntities = $state<StoryEntityContext[]>([]);

	// Load entity context
	$effect(() => {
		if (!isTauri()) return;
		const ipc = (window as any).__TAURI_INTERNALS__;
		ipc?.invoke('get_story_entities', { storyId: story.id })
			.then((e: StoryEntityContext[]) => { storyEntities = e; })
			.catch(() => {});
	});
	let followUpQuery = $state('');
	let conversationEl = $state<HTMLElement | null>(null);

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

		const msgIndex = aiMessages.length;
		aiMessages = [...aiMessages, { question, content: '', sources: [], isStreaming: true }];
		followUpQuery = '';

		let accumulated = '';

		try {
			const streamFn = isTauri()
				? (cb: (event: ChatStreamEvent) => void) => chatSendStream(null, question, cb)
				: (cb: (event: any) => void) => simulateChatStream(cb);

			await streamFn((event: ChatStreamEvent) => {
				if (event.event === 'Delta') {
					accumulated += event.data.text;
					aiMessages[msgIndex].content = accumulated;
				} else if (event.event === 'Complete') {
					aiMessages[msgIndex].content = event.data.message;
					aiMessages[msgIndex].sources = event.data.source_story_ids;
					aiMessages[msgIndex].isStreaming = false;
				} else if (event.event === 'Error') {
					aiMessages[msgIndex].content = accumulated + `\n\nError: ${event.data.message}`;
					aiMessages[msgIndex].isStreaming = false;
				}
			});

			requestAnimationFrame(() => {
				conversationEl?.scrollIntoView({ behavior: 'smooth', block: 'end' });
			});
		} catch (e: any) {
			aiMessages[msgIndex].content = `Error: ${String(e?.message ?? e)}`;
			aiMessages[msgIndex].isStreaming = false;
		} finally {
			isAsking = false;
			aiMessages[msgIndex].isStreaming = false;
		}
	}

	function esc(s: string): string {
		return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
			.replace(/"/g, '&quot;').replace(/'/g, '&#39;');
	}

	function inlineMd(s: string): string {
		let out = esc(s);
		out = out.replace(/\*\*(.+?)\*\*/g, '<strong class="font-semibold text-text">$1</strong>');
		out = out.replace(/\*(.+?)\*/g, '<em class="italic text-text-secondary">$1</em>');
		out = out.replace(/`(.+?)`/g, '<code class="text-xs bg-bg-card-hover px-1 py-0.5 rounded font-mono">$1</code>');
		return out;
	}

	function renderMarkdown(text: string): string {
		return text.split('\n').map(line => {
			const t = line.trim();
			if (t.startsWith('### ')) return `<h4 class="text-xs font-semibold uppercase tracking-wider text-text-muted mt-3 mb-1">${esc(t.slice(4))}</h4>`;
			if (t.startsWith('## ')) return `<h3 class="text-sm font-semibold text-text mt-3 mb-1.5">${inlineMd(t.slice(3))}</h3>`;
			if (t.startsWith('- ')) return `<div class="flex items-start gap-2 ml-1"><span class="text-ai mt-0.5 shrink-0 text-xs">▪</span><span>${inlineMd(t.slice(2))}</span></div>`;
			if (/^\d+\.\s/.test(t)) return `<div class="flex items-start gap-2 ml-1"><span class="text-text-muted mt-0.5 shrink-0 text-xs font-mono">${t.match(/^\d+/)![0]}.</span><span>${inlineMd(t.replace(/^\d+\.\s/, ''))}</span></div>`;
			if (t === '') return '<div class="h-2"></div>';
			return `<p>${inlineMd(t)}</p>`;
		}).join('\n');
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
	<div class="flex items-start gap-2 mb-4">
		<h1 class="text-2xl font-bold text-text leading-tight">
			{story.headline}
		</h1>
		{#if story.summary_depth === 'deep'}
			<span class="shrink-0 mt-1.5 text-[10px] font-medium uppercase tracking-wider px-2 py-0.5 rounded-full bg-ai/15 text-ai border border-ai/25">
				Deep Analysis
			</span>
		{/if}
	</div>

	<!-- Summary -->
	<p class="text-text-secondary text-base leading-relaxed mb-6">
		{story.summary}
	</p>

	<!-- Deep Summary (for high-impact stories) -->
	{#if story.deep_summary}
		<div class="mb-6 bg-bg-card border border-ai/20 rounded-xl p-5" style="border-left: 3px solid var(--color-ai)">
			<div class="flex items-center gap-2 mb-3">
				<div class="w-2 h-2 rounded-full bg-ai"></div>
				<span class="text-[10px] font-medium text-text-muted uppercase tracking-wider">Deep Analysis</span>
			</div>
			<div class="text-sm text-text-secondary leading-relaxed space-y-2">
				{@html renderMarkdown(story.deep_summary)}
			</div>
		</div>
	{/if}

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

	<!-- Entity Context -->
	{#if storyEntities.length > 0}
		<div class="mb-6">
			<h3 class="text-xs font-semibold uppercase tracking-wider text-text-muted mb-2">Key Entities</h3>
			<div class="flex flex-wrap gap-1.5">
				{#each storyEntities as entity}
					{@const trajColors: Record<string, string> = {
						dominant: 'bg-ai/15 text-ai border-ai/30',
						hot: 'bg-amber-500/15 text-amber-400 border-amber-500/30',
						rising: 'bg-emerald-500/15 text-emerald-400 border-emerald-500/30',
						fading: 'bg-red-500/15 text-red-400 border-red-500/30',
					}}
					{@const cls = trajColors[entity.trajectory] ?? 'bg-bg-card text-text-secondary border-border'}
					<span
						class="text-[10px] px-2 py-0.5 rounded border inline-flex items-center gap-1 {cls}"
						title="{entity.entity_type ?? 'entity'} · {entity.trajectory} · sentiment: {entity.sentiment.toFixed(2)}"
					>
						{entity.name}
						{#if entity.trajectory !== 'unknown' && entity.trajectory !== 'dormant'}
							<span class="text-[8px] opacity-70">
								{entity.trajectory === 'dominant' ? '◉' : entity.trajectory === 'hot' ? '▲' : entity.trajectory === 'rising' ? '↗' : '↘'}
							</span>
						{/if}
					</span>
				{/each}
			</div>
		</div>
	{/if}

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

				<!-- Streamed response -->
				<div class="bg-bg-card border border-border rounded-xl overflow-hidden" style="border-left: 3px solid var(--color-ai)">
					<div class="px-5 pt-5 pb-4">
						<div class="flex items-center gap-2 mb-3">
							<div class="w-2 h-2 rounded-full bg-ai {msg.isStreaming ? 'animate-pulse' : ''}"></div>
							<span class="text-[10px] font-medium text-text-muted uppercase tracking-wider">Pulse Analysis</span>
						</div>
						<div class="text-sm text-text-secondary leading-relaxed prose-inline">
							{@html renderMarkdown(msg.content)}
						</div>
					</div>

					{#if msg.sources.length > 0}
						<div class="px-5 pb-4 pt-2 border-t border-border">
							<p class="text-[10px] uppercase tracking-wider text-text-muted mb-2">Sources</p>
							<div class="flex flex-wrap gap-1.5">
								{#each msg.sources.slice(0, 6) as id}
									<span class="text-[10px] px-2 py-0.5 rounded-full bg-ai/10 text-ai font-mono">#{id}</span>
								{/each}
							</div>
						</div>
					{/if}
				</div>
			{/each}

			<!-- Loading indicator (before first token) -->
			{#if isAsking && aiMessages.length > 0 && !aiMessages[aiMessages.length - 1].content}
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
