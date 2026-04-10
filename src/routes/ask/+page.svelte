<script lang="ts">
	import { messages, isStreaming, startNewThread, sendMessage, lastResponse, lastSearchSource } from '$lib/stores/chat';
	import ChatMessage from '$lib/components/chat/ChatMessage.svelte';
	import type { ChatMessage as ChatMessageType } from '$lib/tauri/types';

	let query = $state('');
	let messagesContainer: HTMLElement;

	// Check for prefilled question from story cards
	$effect(() => {
		const prefill = sessionStorage.getItem('pulse_ask_prefill');
		if (prefill) {
			sessionStorage.removeItem('pulse_ask_prefill');
			query = prefill;
			// Auto-submit after a tick
			setTimeout(() => handleSubmit(), 100);
		}
	});

	const suggestions = [
		"What are today's biggest AI developments?",
		"Tell me about Claude and Anthropic news",
		"What's happening in Miami Beach?",
		"What trends are emerging across all sectors?",
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
			<div class="text-center pt-16">
				<div class="text-4xl mb-4">◎</div>
				<h2 class="text-xl font-semibold text-text mb-2">Ask Pulse</h2>
				<p class="text-text-secondary text-sm mb-8">
					Ask questions about your news archive. Pulse searches your briefings and answers with Claude.
				</p>
				<div class="grid grid-cols-1 sm:grid-cols-2 gap-2 max-w-lg mx-auto">
					{#each suggestions as suggestion}
						<button
							class="text-left text-sm bg-bg-card border border-border rounded-lg px-3 py-2.5
								text-text-secondary hover:text-text hover:bg-bg-card-hover transition-colors"
							onclick={() => askSuggestion(suggestion)}
						>
							{suggestion}
						</button>
					{/each}
				</div>
			</div>
		{:else}
			{#each $messages as msg (msg.id)}
				<ChatMessage message={msg} />
			{/each}
			{#if $isStreaming}
				<div class="flex justify-start">
					<div class="bg-bg-card border border-border rounded-2xl rounded-bl-sm px-4 py-3">
						<div class="flex gap-1">
							<div class="w-2 h-2 bg-ai rounded-full animate-bounce" style="animation-delay: 0ms"></div>
							<div class="w-2 h-2 bg-ai rounded-full animate-bounce" style="animation-delay: 150ms"></div>
							<div class="w-2 h-2 bg-ai rounded-full animate-bounce" style="animation-delay: 300ms"></div>
						</div>
					</div>
				</div>
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
				<div class="flex flex-wrap gap-2 pt-2">
					{#each $lastResponse.suggested_followups as followup}
						<button
							class="text-xs bg-bg-card border border-border rounded-lg px-3 py-2
								text-text-secondary hover:text-text hover:bg-bg-card-hover hover:border-ai/30 transition-colors"
							onclick={() => askSuggestion(followup)}
						>
							{followup}
						</button>
					{/each}
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
