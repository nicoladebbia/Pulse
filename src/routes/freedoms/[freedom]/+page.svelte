<script lang="ts">
	import { page } from '$app/stores';
	import type { FreedomsBriefing, FreedomStory } from '$lib/tauri/types';
	import { FREEDOM_CONFIG, type FreedomId } from '$lib/config';
	import { isTauri, mockFreedomsBriefing } from '$lib/tauri/mock';
	import { openExternal } from '$lib/tauri/shell';

	function isFreedomId(value: string): value is FreedomId {
		return value in FREEDOM_CONFIG;
	}

	let freedomParam = $derived($page.params.freedom ?? '');
	let freedom = $derived(isFreedomId(freedomParam) ? freedomParam : null);
	let dateParam = $derived($page.url.searchParams.get('date'));
	let config = $derived(freedom ? FREEDOM_CONFIG[freedom] : null);
	let stories = $state<FreedomStory[]>([]);
	let briefingDate = $state('');
	let isLoading = $state(true);
	let error = $state('');
	let expandedId = $state<number | null>(null);
	let loaded = $state(false);

	let lastFreedom = $state('');
	let lastDate = $state('');

	$effect(() => {
		const currentFreedom = freedomParam;
		const currentDate = dateParam ?? '';
		// NB: do not gate on stories.length — loadStories() writes `stories`, and every
		// array assignment to a $state creates a fresh proxy, so an empty result would
		// re-trigger this effect forever (measured: 200+ runs). Manual retry is the button.
		if (loaded && currentFreedom === lastFreedom && currentDate === lastDate) return;
		lastFreedom = currentFreedom;
		lastDate = currentDate;
		loaded = true;
		loadStories();
	});

	async function loadStories() {
		const cfg = config;
		if (!cfg) {
			stories = [];
			isLoading = false;
			return;
		}
		isLoading = true;
		error = '';
		try {
			if (!isTauri()) {
				stories = mockFreedomsBriefing[cfg.key] ?? [];
				briefingDate = 'mock';
				return;
			}
			const ipc = (window as any).__TAURI_INTERNALS__;
			let briefing: FreedomsBriefing | null;
			if (dateParam) {
				briefing = await ipc.invoke('get_freedoms_by_date', { date: dateParam });
			} else {
				briefing = await ipc.invoke('get_today_freedoms');
			}
			if (briefing) {
				stories = briefing[cfg.key] ?? [];
				briefingDate = briefing.date;
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

	function timeAgo(dateStr: string | null): string {
		if (!dateStr) return '';
		const date = new Date(dateStr);
		const now = new Date();
		const hours = Math.floor((now.getTime() - date.getTime()) / 3600000);
		if (hours < 1) return 'Just now';
		if (hours < 24) return `${hours}h ago`;
		return date.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
	}

	let hero = $derived(stories[0]);
	let rest = $derived(stories.slice(1));

	let impactLabel = $derived(
		freedom === 'wealth' ? 'Impact on Your Wealth' :
		freedom === 'time' ? 'Impact on Your Time' :
		freedom === 'location' ? 'Impact on Your Mobility' :
		freedom === 'whoop' ? 'Impact on Your Recovery' :
		'Impact on Your Health'
	);
</script>

{#if config}
	<div class="min-h-full" style="background: {config.motif}">
		<div class="max-w-5xl mx-auto pt-6 pb-16 px-4">
			<!-- Back link -->
			<a
				href="/freedoms{dateParam ? `?date=${dateParam}` : ''}"
				class="inline-flex items-center gap-2 text-sm text-text-muted hover:text-text transition-colors mb-6"
			>
				← Four Freedoms
			</a>

			{#if dateParam}
				<p class="text-[10px] uppercase tracking-[0.2em] text-text-muted mb-3">
					{new Date(dateParam + 'T12:00:00').toLocaleDateString('en-US', { weekday: 'long', month: 'long', day: 'numeric' })}
				</p>
			{/if}

			<!-- Freedom header with big title and icon -->
			<header class="mb-10 flex items-start gap-4">
				<span class="text-5xl leading-none mt-1" style="color: {config.color}">{config.icon}</span>
				<div class="flex-1">
					<h1 class="text-4xl font-light text-text tracking-tight mb-1" style="color: {config.color}">
						{config.label}
					</h1>
					<p class="text-sm uppercase tracking-[0.25em] text-text-muted mb-3">{config.subtitle}</p>
					<p class="text-sm text-text-secondary leading-relaxed max-w-2xl">{config.description}</p>
				</div>
				<span
					class="text-[10px] font-mono px-3 py-1.5 rounded-full shrink-0"
					style="color: {config.color}; background: {config.dim}"
				>
					{stories.length} stories
				</span>
			</header>

			<div class="h-px mb-10 opacity-40" style="background: linear-gradient(to right, {config.color}, transparent)"></div>

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
				<p class="text-sm text-text-muted italic py-12 text-center">No intelligence gathered today for this freedom.</p>
			{:else}
				<!-- Hero story: huge, prominent -->
				{#if hero}
					<div
						class="rounded-2xl mb-8 transition-all duration-200"
						style="background: {expandedId === hero.id ? config.dim : 'var(--color-bg-card)'}; border-left: 4px solid {config.color}; box-shadow: 0 1px 3px rgba(0,0,0,0.1);"
					>
						<button
							class="block w-full text-left px-8 pt-8 {expandedId === hero.id ? 'pb-6' : 'pb-8'}"
							onclick={() => toggleExpand(hero.id)}
						>
							<div class="flex items-center gap-2 mb-3">
								<span class="text-[10px] uppercase tracking-[0.2em] font-medium" style="color: {config.color}">Top Story</span>
								<span class="text-text-muted opacity-40">·</span>
								<span class="text-[10px] uppercase tracking-wider text-text-muted">{hero.source_name}</span>
								{#if hero.published_at}
									<span class="text-text-muted opacity-40">·</span>
									<span class="text-[10px] uppercase tracking-wider text-text-muted">{timeAgo(hero.published_at)}</span>
								{/if}
							</div>

							<h2 class="text-2xl font-light text-text leading-tight mb-4">
								{hero.headline}
							</h2>
							<p class="text-base text-text-secondary leading-relaxed {expandedId === hero.id ? '' : 'line-clamp-3'}">
								{hero.summary}
							</p>
							{#if expandedId !== hero.id}
								<p class="text-[10px] uppercase tracking-wider text-text-muted mt-4 opacity-60">
									Expand report →
								</p>
							{/if}
						</button>

						{#if expandedId === hero.id}
							<div class="mx-8 pb-8 space-y-5 border-t pt-6" style="border-color: color-mix(in srgb, {config.color} 20%, transparent)">
								{#if hero.key_facts.length > 0}
									<div>
										<h4 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-2">Key Facts</h4>
										<ul class="space-y-1.5">
											{#each hero.key_facts as fact}
												<li class="flex items-start gap-2 text-sm text-text-secondary">
													<span class="mt-1 shrink-0 opacity-50" style="color: {config.color}">·</span>
													<span>{fact}</span>
												</li>
											{/each}
										</ul>
									</div>
								{/if}

								{#if hero.why_it_matters}
									<div class="pl-4 border-l-2" style="border-color: color-mix(in srgb, {config.color} 40%, transparent)">
										<h4 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1.5">{impactLabel}</h4>
										<p class="text-sm text-text-secondary leading-relaxed">{hero.why_it_matters}</p>
									</div>
								{/if}

								{#if hero.what_to_watch}
									<div>
										<h4 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1.5">What to Watch</h4>
										<p class="text-sm text-text-secondary leading-relaxed">{hero.what_to_watch}</p>
									</div>
								{/if}

								<button
									class="text-[10px] uppercase tracking-wider text-text-muted hover:text-text transition-colors"
									onclick={() => openExternal(hero.original_url)}
								>
									Read original source →
								</button>
							</div>
						{/if}
					</div>
				{/if}

				<!-- Smaller stories grid below -->
				{#if rest.length > 0}
					<p class="text-[10px] uppercase tracking-[0.2em] text-text-muted mb-4">More intelligence</p>
					<div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
						{#each rest as story}
							<div
								class="rounded-xl transition-all duration-200 bg-bg-card border hover:scale-[1.01]"
								style="border-color: {expandedId === story.id ? config.color : 'var(--color-border-subtle)'};"
							>
								<button
									class="block w-full text-left p-4"
									onclick={() => toggleExpand(story.id)}
								>
									<div class="flex items-center gap-2 mb-2">
										<span class="w-1 h-1 rounded-full" style="background: {config.color}"></span>
										<span class="text-[10px] uppercase tracking-wider text-text-muted">{story.source_name}</span>
										{#if story.published_at}
											<span class="text-text-muted opacity-40">·</span>
											<span class="text-[10px] text-text-muted">{timeAgo(story.published_at)}</span>
										{/if}
									</div>
									<h3 class="text-sm font-medium text-text leading-snug mb-2 line-clamp-2">
										{story.headline}
									</h3>
									<p class="text-xs text-text-secondary leading-relaxed {expandedId === story.id ? '' : 'line-clamp-2'}">
										{story.summary}
									</p>
								</button>

								{#if expandedId === story.id}
									<div class="mx-4 pb-4 space-y-4 border-t pt-4" style="border-color: color-mix(in srgb, {config.color} 20%, transparent)">
										{#if story.key_facts.length > 0}
											<div>
												<h4 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1.5">Key Facts</h4>
												<ul class="space-y-1">
													{#each story.key_facts as fact}
														<li class="flex items-start gap-1.5 text-xs text-text-secondary">
															<span class="mt-0.5 shrink-0 opacity-50" style="color: {config.color}">·</span>
															<span>{fact}</span>
														</li>
													{/each}
												</ul>
											</div>
										{/if}
										{#if story.why_it_matters}
											<div class="pl-3 border-l-2" style="border-color: color-mix(in srgb, {config.color} 30%, transparent)">
												<h4 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1">{impactLabel}</h4>
												<p class="text-xs text-text-secondary leading-relaxed">{story.why_it_matters}</p>
											</div>
										{/if}
										{#if story.what_to_watch}
											<div>
												<h4 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1">What to Watch</h4>
												<p class="text-xs text-text-secondary leading-relaxed">{story.what_to_watch}</p>
											</div>
										{/if}
										<button
											class="text-[10px] uppercase tracking-wider text-text-muted hover:text-text transition-colors"
											onclick={() => openExternal(story.original_url)}
										>
											Read source →
										</button>
									</div>
								{/if}
							</div>
						{/each}
					</div>
				{/if}
			{/if}

			<!-- Footer -->
			<div class="mt-16 pt-6 border-t border-border-subtle text-center">
				<p class="text-[10px] uppercase tracking-[0.25em] text-text-muted">
					{config.label} Intelligence
				</p>
			</div>
		</div>
	</div>
{:else}
	<div class="flex items-center justify-center h-64">
		<p class="text-text-muted">Unknown freedom: {freedomParam}</p>
	</div>
{/if}
