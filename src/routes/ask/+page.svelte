<script lang="ts">
	import { messages, isStreaming, isSearching, startNewThread, sendMessage, lastResponse, lastSearchSource } from '$lib/stores/chat';
	import ChatMessage from '$lib/components/chat/ChatMessage.svelte';
	import ChatThinking from '$lib/components/chat/ChatThinking.svelte';
	import ChatFollowups from '$lib/components/chat/ChatFollowups.svelte';
	import { getChatContext } from '$lib/tauri/commands';
	import { isTauri } from '$lib/tauri/mock';
	import type { ChatContext } from '$lib/tauri/types';

	let query = $state('');
	let messagesContainer: HTMLElement;
	let chatContext = $state<ChatContext | null>(null);
	let contextLoaded = $state(false);

	// Load dynamic suggestions
	$effect(() => {
		if (contextLoaded) return;
		contextLoaded = true;
		if (isTauri()) {
			getChatContext().then(ctx => { chatContext = ctx; }).catch(() => {});
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

		requestAnimationFrame(() => {
			messagesContainer?.scrollTo({ top: messagesContainer.scrollHeight, behavior: 'smooth' });
		});

		await sendMessage(q);

		requestAnimationFrame(() => {
			messagesContainer?.scrollTo({ top: messagesContainer.scrollHeight, behavior: 'smooth' });
		});
	}

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey) {
			event.preventDefault();
			handleSubmit();
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
			</div>
		{:else}
			{#each $messages as msg (msg.id)}
				<ChatMessage message={msg} />
			{/each}
			{#if $isSearching}
				<ChatThinking />
			{/if}
			{#if !$isStreaming && $lastSearchSource && $lastSearchSource !== 'archive'}
				<div class="text-xs text-text-muted mt-1 mb-2 flex items-center gap-1.5">
					{#if $lastSearchSource === 'web'}
						<span class="w-1.5 h-1.5 rounded-full bg-amber-400"></span> Answered from web search
					{:else if $lastSearchSource === 'archive+web'}
						<span class="w-1.5 h-1.5 rounded-full bg-emerald-400"></span> Answered from archive + web search
					{/if}
				</div>
			{/if}
			{#if !$isStreaming && $lastResponse?.suggested_followups?.length}
				<ChatFollowups followups={$lastResponse.suggested_followups} onAsk={askSuggestion} />
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
			<button
				class="absolute right-3 top-1/2 -translate-y-1/2 text-text-muted hover:text-ai transition-colors
					disabled:opacity-30"
				disabled={!query.trim() || $isStreaming}
				onclick={handleSubmit}
			>
				→
			</button>
		</div>
	</div>
</div>
