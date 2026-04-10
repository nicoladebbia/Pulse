<script lang="ts">
	import { messages, isStreaming, lastResponse, activeThreadId, sendMessage, startNewThread } from '$lib/stores/chat';
	import ChatMessage from './ChatMessage.svelte';
	import ChatInput from './ChatInput.svelte';
	import ChatInsights from './ChatInsights.svelte';

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

			{#if $isStreaming && ($messages[$messages.length - 1]?.content === '')}
				<div class="flex justify-start">
					<div class="bg-bg-card border border-border rounded-2xl rounded-bl-sm px-4 py-3">
						<div class="flex gap-1">
							<div class="w-1.5 h-1.5 bg-ai rounded-full animate-bounce" style="animation-delay: 0ms"></div>
							<div class="w-1.5 h-1.5 bg-ai rounded-full animate-bounce" style="animation-delay: 150ms"></div>
							<div class="w-1.5 h-1.5 bg-ai rounded-full animate-bounce" style="animation-delay: 300ms"></div>
						</div>
					</div>
				</div>
			{/if}

			<!-- Proactive insights -->
			{#if $lastResponse?.proactive_connections?.length}
				<ChatInsights insights={$lastResponse.proactive_connections} />
			{/if}

			<!-- Follow-up suggestions -->
			{#if $lastResponse?.suggested_followups?.length && !$isStreaming}
				<div class="flex flex-wrap gap-1.5 pt-1">
					{#each $lastResponse.suggested_followups as followup}
						<button
							class="text-[11px] bg-ai/10 text-ai px-2.5 py-1 rounded-full
								hover:bg-ai/20 transition-colors"
							onclick={() => sendMessage(followup)}
						>
							{followup}
						</button>
					{/each}
				</div>
			{/if}
		{/if}
	</div>

	<!-- Input -->
	<ChatInput />
</div>
