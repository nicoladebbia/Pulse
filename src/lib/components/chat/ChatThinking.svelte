<script lang="ts">
	import { searchSteps, isSearching } from '$lib/stores/chat';

	// When searching is done, auto-collapse after a short delay
	let collapsed = $state(false);
	let collapseTimer: ReturnType<typeof setTimeout> | null = null;

	$effect(() => {
		if (!$isSearching && $searchSteps.length > 0) {
			collapseTimer = setTimeout(() => { collapsed = true; }, 1500);
		} else {
			collapsed = false;
			if (collapseTimer) clearTimeout(collapseTimer);
		}
	});

	const completedCount = $derived($searchSteps.filter(s => s.done).length);
	const currentStep = $derived($searchSteps[$searchSteps.length - 1]);

	function summaryText(): string {
		const steps = $searchSteps;
		const found = steps.find(s => s.stage === 'found');
		const parts: string[] = [];
		if (found) {
			const match = found.detail.match(/(\d+)/);
			if (match) parts.push(`${match[1]} stories`);
		}
		if (steps.some(s => s.stage === 'web')) parts.push('web');
		if (steps.some(s => s.stage === 'analyzing')) parts.push('signals');
		return parts.length > 0 ? `Searched ${parts.join(' · ')}` : 'Search complete';
	}
</script>

{#if $searchSteps.length > 0}
	<div class="flex justify-start">
		<div class="rounded-2xl rounded-bl-sm px-3.5 py-2.5 max-w-[90%] text-xs">
			{#if collapsed}
				<!-- Collapsed summary -->
				<div class="flex items-center gap-2 text-text-muted">
					<span class="w-1.5 h-1.5 rounded-full bg-emerald-400"></span>
					<span>{summaryText()}</span>
				</div>
			{:else}
				<!-- Step-by-step progress -->
				<div class="space-y-1.5">
					{#each $searchSteps as step}
						<div class="flex items-center gap-2 transition-opacity duration-200"
							class:text-text-muted={step.done}
							class:text-text-secondary={!step.done}>
							{#if step.done}
								<span class="w-1.5 h-1.5 rounded-full bg-emerald-400 shrink-0"></span>
							{:else}
								<span class="w-1.5 h-1.5 rounded-full bg-ai shrink-0 animate-pulse"></span>
							{/if}
							<span>{step.detail}</span>
						</div>
					{/each}
				</div>
			{/if}
		</div>
	</div>
{/if}
