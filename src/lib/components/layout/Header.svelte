<script lang="ts">
	import { currentBriefing, isLoading } from '$lib/stores/briefing';
	import { isFetching, fetchProgress } from '$lib/stores/fetch';

	const today = new Date().toLocaleDateString('en-US', {
		weekday: 'long',
		year: 'numeric',
		month: 'long',
		day: 'numeric'
	});

	// Compute briefing age for staleness display
	function getBriefingAge(): string | null {
		const createdAt = $currentBriefing?.briefing?.created_at;
		if (!createdAt) return null;
		const created = new Date(createdAt + 'Z');
		const hours = Math.floor((Date.now() - created.getTime()) / (1000 * 60 * 60));
		if (hours < 1) return null;
		if (hours === 1) return '1h ago';
		return `${hours}h ago`;
	}
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
		{#if $isFetching}
			<div class="flex items-center gap-2 text-sm text-ai">
				<div class="w-2 h-2 rounded-full bg-ai animate-pulse shadow-[0_0_6px_var(--color-ai)]"></div>
				<span class="header-stage-label">{$fetchProgress?.stage_label ?? 'Fetching...'}</span>
			</div>
		{:else if $currentBriefing}
			{@const age = getBriefingAge()}
			{#if age}
				<div class="flex items-center gap-2 text-sm text-text-muted">
					<div class="w-2 h-2 rounded-full bg-amber-400"></div>
					{age}
				</div>
			{:else}
				<div class="flex items-center gap-2 text-sm text-text-muted">
					<div class="w-2 h-2 rounded-full bg-italy"></div>
					Fresh
				</div>
			{/if}
		{:else}
			<div class="flex items-center gap-2 text-sm text-text-muted">
				<div class="w-2 h-2 rounded-full bg-text-muted"></div>
				Waiting
			</div>
		{/if}
	</div>
</header>

<style>
	.header-stage-label {
		max-width: 180px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		animation: stageFade 0.3s ease-out;
	}

	@keyframes stageFade {
		from { opacity: 0; transform: translateX(4px); }
		to { opacity: 1; transform: translateX(0); }
	}
</style>
