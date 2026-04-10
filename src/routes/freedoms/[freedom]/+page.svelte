<script lang="ts">
	import { page } from '$app/stores';
	import type { FreedomsBriefing, FreedomStory } from '$lib/tauri/types';
	import { FREEDOM_CONFIG } from '$lib/config';
	import { isTauri, mockFreedomsBriefing } from '$lib/tauri/mock';

	let freedom = $derived($page.params.freedom);
	let config = $derived(FREEDOM_CONFIG[freedom]);
	let stories = $state<FreedomStory[]>([]);
	let isLoading = $state(true);
	let error = $state('');
	let expandedId = $state<number | null>(null);
	let loaded = $state(false);

	let lastFreedom = $state('');

	$effect(() => {
		// Re-trigger when freedom param changes (navigation between sub-pages)
		const currentFreedom = freedom;
		if (loaded && stories.length > 0 && currentFreedom === lastFreedom) return;
		lastFreedom = currentFreedom;
		loaded = true;
		loadStories();
	});

	async function loadStories() {
		isLoading = true;
		error = '';
		try {
			const keyMap: Record<string, keyof FreedomsBriefing> = {
				time: 'time_stories', financial: 'financial_stories',
				location: 'location_stories', health: 'health_stories',
			};
			if (!isTauri()) {
				stories = (mockFreedomsBriefing[keyMap[freedom]] as FreedomStory[]) ?? [];
				return;
			}
			const ipc = (window as any).__TAURI_INTERNALS__;
			const briefing: FreedomsBriefing | null = await ipc.invoke('get_today_freedoms');
			if (briefing) {
				stories = (briefing[keyMap[freedom]] as FreedomStory[]) ?? [];
			}
		} catch (e: any) {
			console.error(e);
			error = String(e?.message ?? e);
		} finally {
			isLoading = false;
		}
	}

	function toggleExpand(id: number) {
		expandedId = expandedId === id ? null : id;
	}

	function openSource(url: string) {
		const ipc = (window as any).__TAURI_INTERNALS__;
		if (ipc) ipc.invoke('plugin:shell|open', { path: url });
		else window.open(url, '_blank');
	}

	function timeAgo(dateStr: string | null): string {
		if (!dateStr) return '';
		const date = new Date(dateStr);
		const now = new Date();
		const hours = Math.floor((now.getTime() - date.getTime()) / 3600000);
		if (hours < 1) return 'Just now';
		if (hours < 24) return `${hours}h ago`;
		return date.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
	}
</script>

{#if !config}
	<div class="flex items-center justify-center h-64">
		<p class="text-text-muted">Unknown freedom: {freedom}</p>
	</div>
{:else}
	<!-- Page background gradient — unique per freedom -->
	<div class="min-h-full" style="background: {config.motif}">
		<div class="max-w-2xl mx-auto pt-6 pb-16">

			<!-- Back + nav -->
			<a href="/freedoms" class="inline-flex items-center gap-2 text-sm text-text-muted hover:text-text transition-colors mb-8">
				← Four Freedoms
			</a>

			<!-- Header — unique per freedom -->
			<header class="mb-10">
				<div class="flex items-center gap-3 mb-4">
					<span class="text-3xl" style="color: {config.color}">{config.icon}</span>
					<div>
						<h1 class="text-2xl font-light text-text tracking-wide">{config.label}</h1>
						<p class="text-xs uppercase tracking-[0.2em]" style="color: {config.color}">{config.subtitle}</p>
					</div>
				</div>
				<p class="text-sm text-text-muted leading-relaxed max-w-lg">{config.description}</p>
				<div class="w-16 h-px mt-5 opacity-50" style="background: {config.color}"></div>
			</header>

			{#if isLoading}
				<div class="flex items-center justify-center h-32">
					<div class="w-5 h-5 border rounded-full animate-spin" style="border-color: {config.color}; border-top-color: transparent"></div>
				</div>
			{:else if error}
				<div class="text-center py-12">
					<p class="text-text-muted text-sm mb-4">{error}</p>
					<button
						class="px-4 py-2 text-sm rounded-lg transition-colors"
						style="background: color-mix(in srgb, {config.color} 15%, transparent); color: {config.color}"
						onclick={loadStories}
					>
						Try again
					</button>
				</div>
			{:else if stories.length === 0}
				<p class="text-sm text-text-muted italic">No intelligence gathered today for this freedom.</p>
			{:else}
				<!-- Stories list -->
				<div class="space-y-1">
					{#each stories as story, i}
						{@const isFirst = i === 0}
						<div class="transition-all duration-200"
							 class:mb-6={isFirst && expandedId !== story.id}>

							<!-- Story card -->
							<button
								class="block w-full text-left py-4 transition-colors duration-200"
								class:pl-5={!isFirst}
								class:border-l={!isFirst}
								style={!isFirst ? `border-color: ${expandedId === story.id ? config.color : 'var(--color-border-subtle)'}` : ''}
								onclick={() => toggleExpand(story.id)}
							>
								{#if isFirst}
									<!-- Hero story: bigger, bolder -->
									<div class="rounded-xl p-5 transition-colors duration-200"
										style="background: {expandedId === story.id ? config.dim : 'var(--color-bg-card)'}; border-left: 3px solid {config.color}">
										<h2 class="text-lg font-normal text-text leading-relaxed mb-2">
											{story.headline}
										</h2>
										<p class="text-sm text-text-muted leading-relaxed {expandedId === story.id ? '' : 'line-clamp-3'}">{story.summary}</p>
										{#if story.why_it_matters && expandedId !== story.id}
											<p class="text-xs text-text-muted italic mt-2 line-clamp-1">{story.why_it_matters.split(/[.!?]\s/)[0]}.</p>
										{/if}
										<div class="flex items-center gap-2 text-[10px] text-text-muted uppercase tracking-wider mt-3">
											<span>{story.source_name}</span>
											{#if story.published_at}
												<span class="opacity-40">·</span>
												<span>{timeAgo(story.published_at)}</span>
											{/if}
											<span class="ml-auto opacity-60">{expandedId === story.id ? 'Collapse' : 'Read report'}</span>
										</div>
									</div>
								{:else}
									<!-- Regular story -->
									<div class="flex items-start gap-3">
										<span class="text-[6px] mt-2 shrink-0 transition-opacity"
											style="color: {config.color}; opacity: {expandedId === story.id ? 0.9 : 0.3}">●</span>
										<div>
											<p class="text-sm leading-snug transition-colors"
												style="color: {expandedId === story.id ? 'var(--color-text)' : 'var(--color-text-secondary)'}">
												{story.headline}
											</p>
											<span class="text-[10px] text-text-muted uppercase tracking-wider">{story.source_name}</span>
										</div>
									</div>
								{/if}
							</button>

							<!-- Expanded report -->
							{#if expandedId === story.id}
								<div class="{isFirst ? 'px-5 pb-5 -mt-2' : 'ml-8 pl-4 border-l border-border-subtle mb-4'} space-y-4"
									 style="animation: fadeIn 0.2s ease-out">

									<p class="text-sm text-text-secondary leading-relaxed">{story.summary}</p>

									{#if story.key_facts.length > 0}
										<div>
											<h4 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-2">Key Facts</h4>
											<ul class="space-y-1.5">
												{#each story.key_facts as fact}
													<li class="flex items-start gap-2 text-sm text-text-secondary">
														<span class="mt-1 shrink-0 opacity-40" style="color: {config.color}">·</span>
														<span>{fact}</span>
													</li>
												{/each}
											</ul>
										</div>
									{/if}

									{#if story.why_it_matters}
										<div class="pl-4 border-l-2" style="border-color: color-mix(in srgb, {config.color} 30%, transparent)">
											<h4 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1.5">
												{freedom === 'financial' ? 'Impact on Your Wealth' :
												 freedom === 'time' ? 'Impact on Your Time' :
												 freedom === 'location' ? 'Impact on Your Mobility' :
												 'Impact on Your Health'}
											</h4>
											<p class="text-sm text-text-secondary leading-relaxed">{story.why_it_matters}</p>
										</div>
									{/if}

									{#if story.what_to_watch}
										<div>
											<h4 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1.5">What to Watch</h4>
											<p class="text-sm text-text-secondary leading-relaxed">{story.what_to_watch}</p>
										</div>
									{/if}

									<button
										class="text-[10px] uppercase tracking-wider text-text-muted hover:text-text transition-colors"
										onclick={() => openSource(story.original_url)}
									>
										Read original source →
									</button>
								</div>
							{/if}
						</div>
					{/each}
				</div>
			{/if}

			<!-- Footer -->
			<div class="mt-16 pt-6 border-t border-border-subtle text-center">
				<p class="text-[10px] uppercase tracking-[0.25em] text-text-muted">
					{config.label} Intelligence
				</p>
			</div>
		</div>
	</div>
{/if}

<style>
	@keyframes fadeIn {
		from { opacity: 0; transform: translateY(-4px); }
		to { opacity: 1; transform: translateY(0); }
	}
</style>
