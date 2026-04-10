<script lang="ts">
	import { threads, startNewThread, deleteThread, formatTimestamp, TOPIC_COLORS, TOPIC_LABELS } from '$lib/stores/chat';

	let { onSelect }: { onSelect: (id: string) => void } = $props();

	let groupedThreads = $derived(() => {
		const groups: Record<string, typeof $threads> = {};
		for (const t of $threads) {
			if (!groups[t.topic]) groups[t.topic] = [];
			groups[t.topic].push(t);
		}
		return groups;
	});
</script>

<div class="flex-1 overflow-y-auto">
	<!-- New conversation button -->
	<div class="p-3">
		<button
			class="w-full flex items-center gap-2 px-3 py-2 rounded-lg text-sm text-ai
				bg-ai/10 hover:bg-ai/20 transition-colors"
			onclick={() => { startNewThread(); onSelect(''); }}
		>
			+ New conversation
		</button>
	</div>

	{#if $threads.length === 0}
		<div class="text-center py-8">
			<p class="text-text-muted text-sm">No conversations yet</p>
		</div>
	{:else}
		{#each Object.entries(groupedThreads()) as [topic, topicThreads]}
			<div class="px-3 py-2">
				<p class="text-[10px] font-medium uppercase tracking-wider text-text-muted mb-1.5"
				   style="color: {TOPIC_COLORS[topic] ?? 'var(--color-text-muted)'}">
					{TOPIC_LABELS[topic] ?? topic}
				</p>
				{#each topicThreads as thread}
					<div class="flex items-center group">
						<button
							class="flex-1 text-left px-3 py-2 rounded-lg text-sm hover:bg-bg-card transition-colors"
							onclick={() => onSelect(thread.id)}
						>
							<p class="text-text truncate">{thread.title ?? 'Untitled'}</p>
							<p class="text-[10px] text-text-muted">{formatTimestamp(thread.updated_at)}</p>
						</button>
						<button
							class="opacity-0 group-hover:opacity-100 text-text-muted hover:text-red-400
								transition-opacity px-2 text-xs"
							onclick={() => deleteThread(thread.id)}
						>
							&times;
						</button>
					</div>
				{/each}
			</div>
		{/each}
	{/if}
</div>
