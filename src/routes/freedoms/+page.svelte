<script lang="ts">
	import type { FreedomsBriefing } from '$lib/tauri/types';
	import { FREEDOM_CONFIG, FREEDOM_ORDER } from '$lib/config';
	import { isTauri, mockFreedomsBriefing } from '$lib/tauri/mock';

	let briefing = $state<FreedomsBriefing | null>(null);
	let isLoading = $state(true);
	let error = $state<string | null>(null);
	let loaded = $state(false);

	const freedoms = FREEDOM_ORDER.map(id => FREEDOM_CONFIG[id]);

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadFreedoms();
	});

	async function loadFreedoms() {
		isLoading = true;
		error = null;
		try {
			if (!isTauri()) {
				briefing = mockFreedomsBriefing;
				return;
			}
			const ipc = (window as any).__TAURI_INTERNALS__;
			briefing = await ipc.invoke('get_today_freedoms');
		} catch (e: any) {
			error = String(e?.message ?? e);
		} finally {
			isLoading = false;
		}
	}

	let totalStories = $derived(
		briefing
			? briefing.time_stories.length + briefing.financial_stories.length +
			  briefing.location_stories.length + briefing.health_stories.length
			: 0
	);
</script>

{#if isLoading}
	<div class="flex items-center justify-center h-64">
		<div class="text-center">
			<div class="w-6 h-6 border border-gold border-t-transparent rounded-full animate-spin mx-auto mb-4"></div>
			<p class="text-text-muted text-sm tracking-wide">Loading...</p>
		</div>
	</div>
{:else if error}
	<div class="flex items-center justify-center h-64">
		<div class="text-center">
			<p class="text-text-muted text-sm mb-4">{error}</p>
			<button
				class="px-4 py-2 text-sm bg-gold/10 text-gold rounded-lg hover:bg-gold/20 transition-colors"
				onclick={loadFreedoms}
			>
				Try again
			</button>
		</div>
	</div>
{:else if !briefing}
	<div class="flex items-center justify-center h-64">
		<div class="text-center max-w-sm">
			<div class="w-8 h-px bg-gold mx-auto mb-6"></div>
			<h2 class="text-lg font-light text-text tracking-wide mb-3">No Briefing Yet</h2>
			<p class="text-sm text-text-muted leading-relaxed">
				Your Four Freedoms briefing runs automatically at 8:00 AM.
			</p>
		</div>
	</div>
{:else}
	<div class="max-w-3xl mx-auto pt-6 pb-12">
		<!-- Editorial header -->
		<header class="mb-6 text-center">
			<p class="text-[10px] uppercase tracking-[0.3em] text-gold-muted mb-3">Daily Intelligence</p>
			<h1 class="text-2xl font-light text-text tracking-wide mb-2">The Four Freedoms</h1>
			<p class="text-sm text-text-muted font-light">
				{totalStories} stories across wealth and health
			</p>
			<div class="w-12 h-px bg-gold mx-auto mt-5 opacity-60"></div>
		</header>

		<!-- Executive Summary -->
		{#if briefing.summary}
			<div class="text-center max-w-xl mx-auto mb-10">
				<p class="text-sm leading-relaxed text-text-secondary">
					{briefing.summary}
				</p>
			</div>
		{/if}

		<!-- 4 Freedom Cards — each clickable -->
		<div class="grid grid-cols-2 gap-5">
			{#each freedoms as f}
				{@const stories = briefing[f.key]}
				{@const topStory = stories[0]}
				<a
					href="/freedoms/{f.id}"
					class="group relative block rounded-xl overflow-hidden transition-all duration-300
						hover:scale-[1.02] hover:shadow-lg"
					style="background: linear-gradient(135deg, {f.dim}, var(--color-bg-card))"
				>
					<!-- Top accent line -->
					<div class="h-[2px]" style="background: linear-gradient(to right, {f.color}, transparent)"></div>

					<div class="p-5 min-h-[200px] flex flex-col">
						<!-- Header -->
						<div class="flex items-center justify-between mb-4">
							<div class="flex items-center gap-2.5">
								<span class="text-xl opacity-70" style="color: {f.color}">{f.icon}</span>
								<div>
									<h2 class="text-sm font-medium tracking-wide text-text group-hover:text-white transition-colors">
										{f.label}
									</h2>
									<p class="text-[10px] text-text-muted tracking-wider uppercase">{f.subtitle}</p>
								</div>
							</div>
							<span class="text-[10px] font-mono px-2 py-0.5 rounded-full" style="color: {f.color}; background: {f.dim}">
								{stories.length}
							</span>
						</div>

						<!-- Top story preview -->
						{#if topStory}
							<h3 class="text-sm font-medium text-text mb-1.5 line-clamp-2 flex-shrink-0">
								{topStory.headline}
							</h3>
							<p class="text-xs text-text-secondary leading-relaxed mb-2 line-clamp-2 flex-1">
								{topStory.summary.split(/[.!?]\s/)[0]}.
							</p>
							<p class="text-[10px] text-text-muted">{topStory.source_name}</p>
						{:else}
							<p class="text-sm text-text-muted italic flex-1">No stories today</p>
						{/if}

						<!-- Bottom tagline -->
						<div class="mt-4 pt-3 border-t" style="border-color: color-mix(in srgb, {f.color} 15%, transparent)">
							<div class="flex items-center justify-between">
								<p class="text-[9px] text-text-muted tracking-wider uppercase">{f.tagline}</p>
								<span class="text-text-muted opacity-0 group-hover:opacity-100 transition-opacity text-xs">→</span>
							</div>
						</div>
					</div>
				</a>
			{/each}
		</div>

		<!-- Footer -->
		<div class="mt-12 pt-6 border-t border-border-subtle">
			<p class="text-[10px] uppercase tracking-[0.25em] text-text-muted text-center">
				Curated for your freedom
			</p>
		</div>
	</div>
{/if}
