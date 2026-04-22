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

	function goToToday() {
		selectedDate = todayStr();
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
			briefing = null;
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
{:else}
	<div class="max-w-5xl mx-auto pt-6 pb-12 px-4">
		<!-- Editorial header -->
		<header class="mb-8 text-center">
			<p class="text-[10px] uppercase tracking-[0.3em] text-gold-muted mb-3">Daily Intelligence</p>
			<h1 class="text-2xl font-light text-text tracking-wide mb-2">The Four Freedoms</h1>
			{#if briefing}
				<p class="text-sm text-text-muted font-light">
					{totalStories} stories across time, wealth, location &amp; health
				</p>
			{/if}

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

			{#if !isToday}
				<button
					class="mt-3 text-[10px] uppercase tracking-[0.2em] text-gold hover:text-gold/80 transition-colors"
					onclick={goToToday}
				>
					Go to today →
				</button>
			{/if}

			<div class="w-12 h-px bg-gold mx-auto mt-5 opacity-60"></div>
		</header>

		{#if !briefing}
			<!-- Empty-day state -->
			<div class="flex items-center justify-center py-24">
				<div class="text-center max-w-sm">
					<div class="w-8 h-px bg-gold mx-auto mb-6"></div>
					<h2 class="text-lg font-light text-text tracking-wide mb-3">No Briefing for this Day</h2>
					<p class="text-sm text-text-muted leading-relaxed mb-6">
						Pulse didn't gather intelligence for {formatDisplayDate(selectedDate)}.
					</p>
					<button
						class="px-5 py-2.5 text-sm bg-gold/10 text-gold rounded-lg hover:bg-gold/20 transition-colors tracking-wide"
						onclick={goToToday}
					>
						Go to Today →
					</button>
				</div>
			</div>
		{:else}
			<!-- Executive summary (Today-style prose) -->
			{#if briefing.summary}
				<div class="text-center max-w-2xl mx-auto mb-10">
					<p class="text-sm leading-relaxed text-text-secondary">
						{briefing.summary}
					</p>
				</div>
			{/if}

			<!-- 2x2 grid of freedom cards -->
			<div class="grid grid-cols-1 md:grid-cols-2 gap-5">
				{#each freedoms as f}
					{@const stories = briefing[f.key] as FreedomStory[]}
					{@const hero = stories[0]}
					{@const leads = stories.slice(1, 3)}
					<a
						href="/freedoms/{f.id}{isToday ? '' : `?date=${selectedDate}`}"
						class="group relative block rounded-xl overflow-hidden transition-all duration-300 hover:scale-[1.01] hover:shadow-lg"
						style="background: linear-gradient(135deg, {f.dim}, var(--color-bg-card))"
					>
						<!-- Top accent line -->
						<div class="h-[2px]" style="background: linear-gradient(to right, {f.color}, transparent)"></div>

						<div class="p-6 min-h-[320px] flex flex-col">
							<!-- Header -->
							<div class="flex items-center justify-between mb-5">
								<div class="flex items-center gap-3">
									<span class="text-2xl opacity-80" style="color: {f.color}">{f.icon}</span>
									<div>
										<h2 class="text-base font-medium tracking-wide text-text group-hover:text-white transition-colors">
											{f.label}
										</h2>
										<p class="text-[10px] uppercase tracking-[0.2em] text-text-muted">{f.subtitle}</p>
									</div>
								</div>
								<span
									class="text-[10px] font-mono px-2.5 py-1 rounded-full"
									style="color: {f.color}; background: {f.dim}"
								>
									{stories.length}
								</span>
							</div>

							{#if hero}
								<!-- Hero story -->
								<div class="mb-4">
									<h3 class="text-sm font-medium text-text mb-2 line-clamp-2 leading-snug">
										{hero.headline}
									</h3>
									<p class="text-xs text-text-secondary leading-relaxed line-clamp-2 mb-2">
										{hero.summary.split(/[.!?]\s/)[0]}.
									</p>
									<p class="text-[10px] text-text-muted uppercase tracking-wider">{hero.source_name}</p>
								</div>

								<!-- Lead headlines -->
								{#if leads.length > 0}
									<div class="mt-auto pt-3 border-t space-y-2" style="border-color: color-mix(in srgb, {f.color} 15%, transparent)">
										{#each leads as lead}
											<div class="flex items-start gap-2">
												<span class="text-[6px] mt-2 shrink-0 opacity-50" style="color: {f.color}">●</span>
												<p class="text-xs text-text-secondary leading-snug line-clamp-2 flex-1">
													{lead.headline}
												</p>
											</div>
										{/each}
									</div>
								{/if}
							{:else}
								<p class="text-sm text-text-muted italic flex-1">No stories today</p>
							{/if}

							<!-- Bottom tagline -->
							<div class="mt-4 pt-3 border-t flex items-center justify-between" style="border-color: color-mix(in srgb, {f.color} 15%, transparent)">
								<p class="text-[9px] text-text-muted tracking-wider uppercase">{f.tagline}</p>
								<span class="text-text-muted opacity-0 group-hover:opacity-100 transition-opacity text-xs">→</span>
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
		{/if}
	</div>
{/if}
