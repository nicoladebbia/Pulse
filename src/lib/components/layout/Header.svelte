<script lang="ts">
	import { currentBriefing, isLoading } from '$lib/stores/briefing';

	const today = new Date().toLocaleDateString('en-US', {
		weekday: 'long',
		year: 'numeric',
		month: 'long',
		day: 'numeric'
	});

	$effect(() => {
		// Reactively update story count
	});
</script>

<header class="px-8 py-5 border-b border-border flex items-center justify-between shrink-0">
	<div>
		<h1 class="text-lg font-semibold text-text">{today}</h1>
		{#if $currentBriefing}
			<p class="text-sm text-text-secondary mt-0.5">
				{$currentBriefing.briefing.story_count} stories across 4 sectors
			</p>
		{:else if $isLoading}
			<p class="text-sm text-text-muted mt-0.5">Loading briefing...</p>
		{:else}
			<p class="text-sm text-text-muted mt-0.5">No briefing yet today</p>
		{/if}
	</div>

	<div class="flex items-center gap-3">
		<!-- Fetch status indicator -->
		{#if $isLoading}
			<div class="flex items-center gap-2 text-sm text-text-muted">
				<div class="w-2 h-2 rounded-full bg-miami animate-pulse"></div>
				Fetching...
			</div>
		{:else if $currentBriefing}
			<div class="flex items-center gap-2 text-sm text-text-muted">
				<div class="w-2 h-2 rounded-full bg-italy"></div>
				Fresh
			</div>
		{:else}
			<div class="flex items-center gap-2 text-sm text-text-muted">
				<div class="w-2 h-2 rounded-full bg-text-muted"></div>
				Waiting
			</div>
		{/if}
	</div>
</header>
