<script lang="ts">
	import type { ProactiveInsight } from '$lib/tauri/types';

	let { insights }: { insights: ProactiveInsight[] } = $props();
</script>

{#if insights.length > 0}
	<div class="bg-ai/5 border border-ai/20 rounded-xl p-3 mt-1">
		<p class="text-[10px] font-medium uppercase tracking-wider text-ai mb-2">
			Related from your archive
		</p>
		<div class="space-y-2">
			{#each insights as insight}
				<div class="flex items-start gap-2">
					<span class="text-ai text-xs mt-0.5 shrink-0">~</span>
					<div>
						<p class="text-xs text-text leading-snug">{insight.headline}</p>
						<p class="text-[10px] text-text-muted">{insight.date} — {insight.connection}</p>
					</div>
				</div>
			{/each}
		</div>
	</div>
{/if}
