<script lang="ts">
	import { isStreaming, sendMessage, activeThreadId, startNewThread } from '$lib/stores/chat';

	let query = $state('');

	function handleKeydown(event: KeyboardEvent) {
		if (event.key === 'Enter' && !event.shiftKey) {
			event.preventDefault();
			handleSubmit();
		}
	}

	function handleSubmit() {
		const q = query.trim();
		if (!q || $isStreaming) return;
		sendMessage(q);
		query = '';
	}
</script>

<div class="shrink-0 px-4 py-3 border-t border-border">
	{#if $activeThreadId}
		<button
			class="text-[10px] text-text-muted hover:text-text mb-2 transition-colors"
			onclick={startNewThread}
		>
			+ New conversation
		</button>
	{/if}
	<div class="relative">
		<textarea
			bind:value={query}
			onkeydown={handleKeydown}
			placeholder="Ask about your archive..."
			disabled={$isStreaming}
			rows="1"
			class="w-full bg-bg-card border border-border rounded-xl px-3.5 py-2.5 pr-10 text-text text-sm
				placeholder:text-text-muted focus:outline-none focus:border-ai transition-colors
				disabled:opacity-50 resize-none min-h-[40px] max-h-[120px]"
		></textarea>
		<button
			class="absolute right-2.5 bottom-2.5 text-text-muted hover:text-ai transition-colors
				disabled:opacity-30"
			aria-label="Send message"
			disabled={!query.trim() || $isStreaming}
			onclick={handleSubmit}
		>
			<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<path d="M22 2L11 13M22 2L15 22L11 13M22 2L2 9L11 13" />
			</svg>
		</button>
	</div>
</div>
