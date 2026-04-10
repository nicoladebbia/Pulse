<script lang="ts">
	let { followups, onAsk }: { followups: string[]; onAsk: (q: string) => void } = $props();

	interface ParsedFollowup {
		type: string;
		question: string;
		icon: string;
		label: string;
	}

	const TYPE_META: Record<string, { icon: string; label: string }> = {
		deeper: { icon: '🔍', label: 'Go deeper' },
		compare: { icon: '⚖', label: 'Compare' },
		predict: { icon: '🔮', label: 'Predict' },
		timeline: { icon: '📈', label: 'Timeline' },
	};

	const parsed = $derived(
		followups.map((f): ParsedFollowup => {
			const match = f.match(/^(deeper|compare|predict|timeline)::(.*)/);
			if (match) {
				const meta = TYPE_META[match[1]] ?? { icon: '→', label: match[1] };
				return { type: match[1], question: match[2], ...meta };
			}
			return { type: 'general', question: f, icon: '→', label: '' };
		})
	);

	// Group by type
	const grouped = $derived.by(() => {
		const groups: Record<string, ParsedFollowup[]> = {};
		for (const f of parsed) {
			const key = f.type;
			if (!groups[key]) groups[key] = [];
			groups[key].push(f);
		}
		return groups;
	});

	const hasTypes = $derived(parsed.some(f => f.type !== 'general'));
</script>

{#if followups.length > 0}
	<div class="pt-2 space-y-2">
		{#if hasTypes}
			{#each Object.entries(grouped) as [type, items]}
				<div class="flex flex-wrap gap-1.5">
					{#each items as f}
						<button
							class="text-xs bg-bg-card border border-border rounded-lg px-3 py-2
								text-text-secondary hover:text-text hover:bg-bg-card-hover hover:border-ai/30 transition-colors
								flex items-center gap-1.5"
							onclick={() => onAsk(f.question)}
						>
							<span class="text-[10px]">{f.icon}</span>
							<span>{f.question}</span>
						</button>
					{/each}
				</div>
			{/each}
		{:else}
			<div class="flex flex-wrap gap-1.5">
				{#each parsed as f}
					<button
						class="text-xs bg-bg-card border border-border rounded-lg px-3 py-2
							text-text-secondary hover:text-text hover:bg-bg-card-hover hover:border-ai/30 transition-colors"
						onclick={() => onAsk(f.question)}
					>
						{f.question}
					</button>
				{/each}
			</div>
		{/if}
	</div>
{/if}
