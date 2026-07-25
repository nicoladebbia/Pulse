<script lang="ts">
	import { messages, isStreaming, isSearching, startNewThread, sendMessage, cancelStream, regenerateLastMessage, lastResponse } from '$lib/stores/chat';
	import ChatMessage from '$lib/components/chat/ChatMessage.svelte';
	import ChatThinking from '$lib/components/chat/ChatThinking.svelte';
	import ChatInsights from '$lib/components/chat/ChatInsights.svelte';
	import { getChatContext, safeInvoke, safeCall } from '$lib/tauri/commands';
	import { isTauri } from '$lib/tauri/mock';
	import type { ChatContext } from '$lib/tauri/types';

	let query = $state('');
	let messagesContainer: HTMLElement;
	let chatContext = $state<ChatContext | null>(null);
	let feedbackStats = $state<any>(null);
	let contextLoaded = $state(false);

	// Load dynamic suggestions and feedback stats
	$effect(() => {
		if (contextLoaded) return;
		contextLoaded = true;
		if (isTauri()) {
			safeCall(getChatContext).then(ctx => { chatContext = ctx; });
			safeInvoke<any>('get_feedback_stats').then(s => { feedbackStats = s; });
		}
	});

	// Check for prefilled question from story cards
	$effect(() => {
		const prefill = sessionStorage.getItem('pulse_ask_prefill');
		if (prefill) {
			sessionStorage.removeItem('pulse_ask_prefill');
			query = prefill;
			setTimeout(() => handleSubmit(), 100);
		}
	});

	// Auto-scroll to bottom during searching and streaming
	$effect(() => {
		const _ = $messages;
		const __ = $isSearching;
		if (($isStreaming || $isSearching) && messagesContainer) {
			requestAnimationFrame(() => {
				messagesContainer.scrollTo({ top: messagesContainer.scrollHeight, behavior: 'instant' });
			});
		}
	});

	const CATEGORY_ICONS: Record<string, string> = {
		signal: '📡',
		story: '📰',
		prediction: '🔮',
		general: '→',
	};

	const fallbackSuggestions = [
		"What are today's biggest AI developments?",
		"What's happening in Miami Beach?",
		"What trends are emerging across all sectors?",
		"Any patterns between AI and regulation?",
	];

	async function handleSubmit() {
		const q = query.trim();
		if (!q || $isStreaming) return;

		query = '';

		// Scroll to bottom immediately after user message appears
		requestAnimationFrame(() => {
			messagesContainer?.scrollTo({ top: messagesContainer.scrollHeight, behavior: 'instant' });
		});

		await sendMessage(q);

		// Final scroll after streaming completes
		requestAnimationFrame(() => {
			messagesContainer?.scrollTo({ top: messagesContainer.scrollHeight, behavior: 'smooth' });
		});
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey) {
			event.preventDefault();
			handleSubmit();
		} else if (event.key === 'Escape') {
			event.preventDefault();
			if ($isStreaming) {
				cancelStream();
			} else {
				query = '';
			}
		} else if (event.key === 'ArrowUp' && !query) {
			// Recall last user message
			const lastUser = [...$messages].reverse().find(m => m.role === 'user');
			if (lastUser) {
				event.preventDefault();
				query = lastUser.content;
			}
		}
	}

	function askSuggestion(s: string) {
		query = s;
		handleSubmit();
	}

	function clearChat() {
		startNewThread();
	}
</script>

<div class="flex flex-col h-full max-w-2xl mx-auto">
	<!-- Messages -->
	<div class="flex-1 overflow-y-auto space-y-4 py-4" bind:this={messagesContainer}>
		{#if $messages.length === 0}
			<div class="text-center pt-12">
				<div class="w-3 h-3 rounded-full bg-ai mx-auto mb-4 animate-pulse"></div>

				{#if chatContext}
					<h2 class="text-lg font-semibold text-text mb-1">{chatContext.greeting}</h2>
					{#if chatContext.entity_count > 0}
						<p class="text-text-muted text-xs mb-8">
							Tracking {chatContext.entity_count} entities across {chatContext.briefing_days} days
						</p>
					{/if}

					<div class="grid grid-cols-1 sm:grid-cols-2 gap-2 max-w-lg mx-auto">
						{#each chatContext.suggestions as suggestion}
							<button
								class="text-left text-sm bg-bg-card border border-border rounded-lg px-3 py-2.5
									text-text-secondary hover:text-text hover:bg-bg-card-hover transition-colors
									flex items-center gap-2"
								onclick={() => askSuggestion(suggestion.text)}
							>
								<span class="text-xs shrink-0">{CATEGORY_ICONS[suggestion.category] ?? '→'}</span>
								<span>{suggestion.text}</span>
							</button>
						{/each}
					</div>
				{:else}
					<h2 class="text-xl font-semibold text-text mb-2">Ask Pulse</h2>
					<p class="text-text-secondary text-sm mb-8">
						Ask questions about your news archive. Pulse searches your briefings and answers with Claude.
					</p>
					<div class="grid grid-cols-1 sm:grid-cols-2 gap-2 max-w-lg mx-auto">
						{#each fallbackSuggestions as suggestion}
							<button
								class="text-left text-sm bg-bg-card border border-border rounded-lg px-3 py-2.5
									text-text-secondary hover:text-text hover:bg-bg-card-hover transition-colors"
								onclick={() => askSuggestion(suggestion)}
							>
								{suggestion}
							</button>
						{/each}
					</div>
				{/if}

				<!-- Feedback stats: most reliable sources -->
				{#if feedbackStats?.top_sources?.length > 0}
					<div class="max-w-sm mx-auto mt-8 bg-bg-card border border-border rounded-xl p-3">
						<p class="text-[10px] font-medium uppercase tracking-wider text-text-muted mb-2">Your Most Reliable Sources</p>
						<div class="space-y-1.5">
							{#each feedbackStats.top_sources.slice(0, 5) as source}
								{@const repPct = Math.round((source.reputation ?? 0.5) * 100)}
								<div class="flex items-center justify-between">
									<span class="text-xs text-text-secondary truncate mr-2">{source.name}</span>
									<div class="flex items-center gap-2">
										<div class="w-12 h-1 bg-border rounded-full overflow-hidden">
											<div
												class="h-full rounded-full {repPct >= 70 ? 'bg-emerald-400' : repPct >= 50 ? 'bg-amber-400' : 'bg-rose-400'}"
												style="width: {repPct}%"
											></div>
										</div>
										<span class="text-[10px] font-mono text-text-muted w-7 text-right">{repPct}%</span>
									</div>
								</div>
							{/each}
						</div>
						{#if feedbackStats.total_up + feedbackStats.total_down > 0}
							<p class="text-[9px] text-text-muted mt-2 text-center">
								Based on {feedbackStats.total_up + feedbackStats.total_down} feedback ratings
							</p>
						{/if}
					</div>
				{/if}
			</div>
		{:else}
			{#each $messages as msg, i (msg.id)}
				{@const msgIsLast = i === $messages.length - 1 && msg.role === 'assistant'}
				{@const msgSearchSource = (msg.metadata as Record<string, unknown>)?.search_source as string ?? ''}
				<ChatMessage
					message={msg}
					isLast={msgIsLast}
					followups={msgIsLast && !$isStreaming ? ($lastResponse?.suggested_followups ?? []) : []}
					searchSource={!$isStreaming ? msgSearchSource : ''}
					onAsk={askSuggestion}
				/>
			{/each}
			{#if $isSearching}
				<ChatThinking />
			{/if}
			{#if !$isStreaming && $lastResponse?.proactive_connections?.length}
				<div class="px-4">
					<ChatInsights insights={$lastResponse.proactive_connections} />
				</div>
			{/if}
		{/if}
	</div>

	<!-- Input -->
	<div class="shrink-0 pb-4 pt-2 border-t border-border">
		{#if $messages.length > 0}
			<button class="text-xs text-text-muted hover:text-text mb-2" onclick={clearChat}>
				Clear conversation
			</button>
		{/if}
		<div class="relative">
			<input
				type="text"
				bind:value={query}
				onkeydown={handleKeydown}
				placeholder="Ask about your news archive..."
				disabled={$isStreaming}
				data-search-input
				class="w-full bg-bg-card border border-border rounded-xl px-4 py-3 pr-12 text-text text-sm
					placeholder:text-text-muted focus:outline-none focus:border-ai transition-colors
					disabled:opacity-50"
			/>
			{#if $isStreaming}
				<button
					class="absolute right-3 top-1/2 -translate-y-1/2 text-text-muted hover:text-red-400 transition-colors"
					onclick={cancelStream}
					title="Stop generating (Esc)"
				>
					<svg viewBox="0 0 16 16" fill="currentColor" class="w-4 h-4">
						<rect x="3" y="3" width="10" height="10" rx="2" />
					</svg>
				</button>
			{:else}
				<button
					class="absolute right-3 top-1/2 -translate-y-1/2 text-text-muted hover:text-ai transition-colors
						disabled:opacity-30"
					disabled={!query.trim()}
					onclick={handleSubmit}
				>
					→
				</button>
			{/if}
		</div>
	</div>
</div>
