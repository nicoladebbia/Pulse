<script lang="ts">
	import { panelOpen, togglePanel, loadThreads, loadThread, activeThreadId } from '$lib/stores/chat';
	import ChatThreadList from './ChatThreadList.svelte';
	import ChatConversation from './ChatConversation.svelte';

	let showThreadList = $state(false);

	$effect(() => {
		if ($panelOpen) {
			loadThreads();
		}
	});
</script>

{#if $panelOpen}
	<!-- Backdrop -->
	<button
		class="fixed inset-0 bg-black/20 backdrop-blur-[1px] z-40"
		onclick={togglePanel}
		aria-label="Close chat"
	></button>

	<!-- Panel -->
	<div class="fixed right-0 top-0 bottom-0 w-[400px] bg-bg border-l border-border z-50
		flex flex-col shadow-2xl animate-slide-in">

		<!-- Header -->
		<div class="flex items-center justify-between px-4 py-3 border-b border-border shrink-0">
			<div class="flex items-center gap-2">
				<div class="w-2 h-2 rounded-full bg-ai animate-pulse"></div>
				<span class="text-sm font-semibold text-text">Pulse Intelligence</span>
			</div>
			<div class="flex items-center gap-2">
				<button
					class="text-xs text-text-muted hover:text-text transition-colors px-2 py-1 rounded hover:bg-bg-card"
					onclick={() => showThreadList = !showThreadList}
				>
					{showThreadList ? 'Chat' : 'Threads'}
				</button>
				<button
					class="text-text-muted hover:text-text transition-colors text-lg leading-none"
					onclick={togglePanel}
				>
					&times;
				</button>
			</div>
		</div>

		<!-- Content -->
		{#if showThreadList}
			<ChatThreadList onSelect={(id) => { if (id) loadThread(id); showThreadList = false; }} />
		{:else}
			<ChatConversation />
		{/if}
	</div>
{/if}

<style>
	@keyframes slide-in {
		from { transform: translateX(100%); }
		to { transform: translateX(0); }
	}
	.animate-slide-in {
		animation: slide-in 0.2s ease-out;
	}
</style>
