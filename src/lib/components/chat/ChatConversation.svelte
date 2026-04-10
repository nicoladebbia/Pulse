<script lang="ts">
	import { messages, isStreaming, isSearching, lastResponse, activeThreadId, sendMessage, startNewThread } from '$lib/stores/chat';
	import ChatMessage from './ChatMessage.svelte';
	import ChatInput from './ChatInput.svelte';
	import ChatInsights from './ChatInsights.svelte';
	import ChatThinking from './ChatThinking.svelte';
	import ChatFollowups from './ChatFollowups.svelte';

	let messagesContainer: HTMLElement;

	// Auto-scroll on new messages and during streaming
	$effect(() => {
		// Track both message count and last message content for streaming scroll
		const lastMsg = $messages[$messages.length - 1];
		const _trigger = lastMsg?.content;
		if ($messages.length > 0) {
			requestAnimationFrame(() => {
				messagesContainer?.scrollTo({ top: messagesContainer.scrollHeight, behavior: 'smooth' });
			});
		}
	});

	const suggestions = [
		"What's the biggest AI news this week?",
		"What's heating up across all sectors?",
		"Any patterns emerging between AI and regulation?",
		"What are your predictions for the next month?",
	];
</script>

<div class="flex-1 flex flex-col overflow-hidden">
	<!-- Messages -->
	<div class="flex-1 overflow-y-auto px-4 py-3 space-y-3" bind:this={messagesContainer}>
		{#if $messages.length === 0}
			<div class="text-center pt-12">
				<div class="w-3 h-3 rounded-full bg-ai mx-auto mb-4 animate-pulse"></div>
				<h3 class="text-sm font-semibold text-text mb-1">Ask Pulse anything</h3>
				<p class="text-xs text-text-muted mb-6">
					I have deep memory of your entire news archive.
				</p>
				<div class="space-y-2 max-w-xs mx-auto">
					{#each suggestions as suggestion}
						<button
							class="w-full text-left text-xs bg-bg-card border border-border rounded-lg px-3 py-2
								text-text-secondary hover:text-text hover:bg-bg-card-hover transition-colors"
							onclick={() => sendMessage(suggestion)}
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

			{#if $isSearching}
				<ChatThinking />
			{/if}

			<!-- Proactive insights -->
			{#if $lastResponse?.proactive_connections?.length}
				<ChatInsights insights={$lastResponse.proactive_connections} />
			{/if}

			<!-- Follow-up suggestions -->
			{#if $lastResponse?.suggested_followups?.length && !$isStreaming}
				<ChatFollowups followups={$lastResponse.suggested_followups} onAsk={sendMessage} />
			{/if}
		{/if}
	</div>

	<!-- Input -->
	<ChatInput />
</div>
