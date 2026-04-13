<script lang="ts">
	import { page } from '$app/stores';
	import type { FreedomsBriefing, FreedomStory } from '$lib/tauri/types';
	import { FREEDOM_CONFIG, FREEDOM_ORDER } from '$lib/config';
	import { isTauri, mockFreedomsBriefing } from '$lib/tauri/mock';

	let briefing = $state<FreedomsBriefing | null>(null);
	let isLoading = $state(true);
	let error = $state<string | null>(null);
	let loaded = $state(false);
	let selectedDate = $state($page.url.searchParams.get('date') ?? todayStr());

	const freedoms = FREEDOM_ORDER.map(id => FREEDOM_CONFIG[id]);

	function todayStr(): string {
		const d = new Date();
		return d.toISOString().slice(0, 10);
	}

	function formatDisplayDate(dateStr: string): string {
		const d = new Date(dateStr + 'T12:00:00');
		const today = todayStr();
		if (dateStr === today) return 'Today';
		const yesterday = new Date();
		yesterday.setDate(yesterday.getDate() - 1);
		if (dateStr === yesterday.toISOString().slice(0, 10)) return 'Yesterday';
		return d.toLocaleDateString('en-US', { weekday: 'short', month: 'short', day: 'numeric' });
	}

	function shiftDate(days: number) {
		const d = new Date(selectedDate + 'T12:00:00');
		d.setDate(d.getDate() + days);
		const newDate = d.toISOString().slice(0, 10);
		if (newDate > todayStr()) return;
		selectedDate = newDate;
		loadFreedoms();
	}

	let isToday = $derived(selectedDate === todayStr());

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
			if (selectedDate === todayStr()) {
				briefing = await ipc.invoke('get_today_freedoms');
			} else {
				briefing = await ipc.invoke('get_freedoms_by_date', { date: selectedDate });
			}
		} catch (e: any) {
			error = String(e?.message ?? e);
		} finally {
			isLoading = false;
		}
	}

	let totalStories = $derived(
		briefing
			? briefing.time_stories.length + briefing.wealth_stories.length +
			  briefing.location_stories.length + briefing.health_stories.length
			: 0
	);

	// Hero stories — first story from each freedom that has stories
	let heroStories = $derived<{ story: FreedomStory; config: typeof FREEDOM_CONFIG[keyof typeof FREEDOM_CONFIG] }[]>(
		briefing
			? freedoms
				.filter(f => (briefing![f.key] as FreedomStory[]).length > 0)
				.map(f => ({ story: (briefing![f.key] as FreedomStory[])[0], config: f }))
			: []
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

			<!-- Date navigation -->
			<div class="flex items-center justify-center gap-4 mt-4">
				<button
					class="text-text-muted hover:text-text transition-colors p-1"
					onclick={() => shiftDate(-1)}
					aria-label="Previous day"
				>←</button>
				<span class="text-sm text-text-secondary font-light min-w-[120px]">
					{formatDisplayDate(selectedDate)}
				</span>
				<button
					class="text-text-muted hover:text-text transition-colors p-1 disabled:opacity-20 disabled:cursor-default"
					onclick={() => shiftDate(1)}
					disabled={isToday}
					aria-label="Next day"
				>→</button>
			</div>
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
					href="/freedoms/{f.id}{isToday ? '' : `?date=${selectedDate}`}"
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

		<!-- Today's Highlights — hero story from each freedom -->
		{#if heroStories.length > 0}
			<div class="mt-10">
				<h2 class="text-[10px] uppercase tracking-[0.25em] text-text-muted mb-5 text-center">Highlights</h2>
				<div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
					{#each heroStories as { story, config: fc }}
						<a
							href="/freedoms/{fc.id}{isToday ? '' : `?date=${selectedDate}`}"
							class="group block rounded-xl overflow-hidden transition-all duration-200 hover:scale-[1.01] bg-bg-card border border-border-subtle"
						>
							<div class="h-[2px]" style="background: linear-gradient(to right, {fc.color}, transparent)"></div>
							<div class="p-4">
								<div class="flex items-center gap-2 mb-2">
									<span class="text-sm opacity-60" style="color: {fc.color}">{fc.icon}</span>
									<span class="text-[10px] uppercase tracking-wider text-text-muted">{fc.label}</span>
								</div>
								<h3 class="text-sm font-medium text-text leading-snug mb-1.5 line-clamp-2 group-hover:text-white transition-colors">
									{story.headline}
								</h3>
								<p class="text-xs text-text-secondary leading-relaxed line-clamp-2">
									{story.summary.split(/[.!?]\s/)[0]}.
								</p>
								<p class="text-[10px] text-text-muted mt-2">{story.source_name}</p>
							</div>
						</a>
					{/each}
				</div>
			</div>
		{/if}

		<!-- Footer -->
		<div class="mt-12 pt-6 border-t border-border-subtle">
			<p class="text-[10px] uppercase tracking-[0.25em] text-text-muted text-center">
				Curated for your freedom
			</p>
		</div>
	</div>
{/if}
