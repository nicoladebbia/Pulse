<script lang="ts">
	import HeroCard from '$lib/components/stories/HeroCard.svelte';
	import StoryCard from '$lib/components/stories/StoryCard.svelte';
	import StoryExpanded from '$lib/components/stories/StoryExpanded.svelte';
	import FreedomSection from '$lib/components/freedoms/FreedomSection.svelte';
	import { SECTORS, type SectorId } from '$lib/config';
	import type { BriefingWithStories, Briefing, Story, FreedomsBriefing } from '$lib/tauri/types';
	import { isTauri, mockArchiveBriefings, mockBriefingData, mockFreedomsBriefing } from '$lib/tauri/mock';
	import { safeInvoke } from '$lib/tauri/commands';
	import { byRankDesc, isFiling } from '$lib/stores/briefing';

	let briefings = $state<Briefing[]>([]);
	let selectedBriefing = $state<BriefingWithStories | null>(null);
	let selectedFreedoms = $state<FreedomsBriefing | null>(null);
	let expandedStory = $state<Story | null>(null);
	let loading = $state(true);
	let loaded = $state(false);
	let error = $state<string | null>(null);
	let viewMode = $state<'list' | 'daily' | 'freedoms'>('list');
	let dateRange = $state<'7' | '30' | 'all'>('all');

	// Timeline windowing: render newest PAGE_SIZE days, reveal more as the
	// sentinel scrolls into view. Client-side only — list_briefings returns
	// metadata rows, so the full fetch stays cheap; this is render windowing.
	const PAGE_SIZE = 15;
	let visibleDays = $state(PAGE_SIZE);
	let sentinel = $state<HTMLElement | null>(null);

	$effect(() => {
		void dateRange; // reset the window whenever the range filter changes
		visibleDays = PAGE_SIZE;
	});

	$effect(() => {
		if (!sentinel) return;
		const obs = new IntersectionObserver(
			(entries) => {
				if (entries[0].isIntersecting) visibleDays += PAGE_SIZE;
			},
			{ rootMargin: '400px' }
		);
		obs.observe(sentinel);
		return () => obs.disconnect();
	});

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadArchive();
	});

	async function loadArchive() {
		loading = true;
		error = null;
		if (!isTauri()) {
			briefings = mockArchiveBriefings;
			openFromQueryParam();
			loading = false;
			return;
		}
		const result = await safeInvoke<Briefing[]>('list_briefings');
		if (result === null) {
			error = 'Failed to load archive';
		} else {
			briefings = result;
			openFromQueryParam();
		}
		loading = false;
	}

	// Deep-link support: /archive?date=today (or ?date=YYYY-MM-DD) opens that
	// day's briefing directly instead of landing on the day-picker list.
	function openFromQueryParam() {
		const param = new URLSearchParams(window.location.search).get('date');
		if (!param) return;
		// Local date, not toISOString() (UTC) — after 8 PM Miami the UTC date is
		// already tomorrow and the lookup would silently miss today's briefing.
		const now = new Date();
		const localToday = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
		const date = param === 'today' ? localToday : param;
		const target = briefings.find(b => b.date === date && b.briefing_type !== 'freedoms');
		if (target) selectDaily(target.id);
	}

	async function selectDaily(briefingId: number) {
		if (!isTauri()) {
			selectedBriefing = mockBriefingData;
			selectedFreedoms = null;
			viewMode = 'daily';
			return;
		}
		const result = await safeInvoke<BriefingWithStories>('get_briefing_by_id', { briefingId });
		if (result) {
			selectedBriefing = result;
			selectedFreedoms = null;
			viewMode = 'daily';
		}
	}

	async function selectFreedoms(date: string) {
		if (!isTauri()) {
			selectedFreedoms = mockFreedomsBriefing;
			selectedBriefing = null;
			viewMode = 'freedoms';
			return;
		}
		const result = await safeInvoke<FreedomsBriefing>('get_freedoms_by_date', { date });
		if (result) {
			selectedFreedoms = result;
			selectedBriefing = null;
			viewMode = 'freedoms';
		}
	}

	function backToList() {
		viewMode = 'list';
		selectedBriefing = null;
		selectedFreedoms = null;
		expandedStory = null;
	}

	function formatDate(date: string): string {
		return new Date(date + 'T12:00:00').toLocaleDateString('en-US', {
			weekday: 'long', year: 'numeric', month: 'long', day: 'numeric'
		});
	}

	function formatDayShort(date: string): string {
		return new Date(date + 'T12:00:00').toLocaleDateString('en-US', {
			weekday: 'short', month: 'short', day: 'numeric'
		});
	}

	function monthLabel(date: string): string {
		return new Date(date + 'T12:00:00').toLocaleDateString('en-US', {
			month: 'long', year: 'numeric'
		});
	}

	function isToday(date: string): boolean {
		return date === new Date().toISOString().split('T')[0];
	}

	function truncateSentences(text: string, count: number): string {
		const sentences = text.match(/[^.!?]+[.!?]+/g);
		if (!sentences) return text;
		return sentences.slice(0, count).join(' ').trim();
	}

	const sectorNames: Record<string, string> = { ai: 'AI', miami: 'Miami', italy: 'Italy', tech: 'Tech' };

	// Group briefings by date — supports multiple daily briefings
	const groupedByDate = $derived.by(() => {
		const groups: Record<string, { dailies: Briefing[]; freedoms?: Briefing }> = {};
		for (const b of briefings) {
			if (!groups[b.date]) groups[b.date] = { dailies: [] };
			if (b.briefing_type === 'freedoms') groups[b.date].freedoms = b;
			else groups[b.date].dailies.push(b);
		}
		return Object.entries(groups).sort((a, b) => b[0].localeCompare(a[0]));
	});

	const filteredByDate = $derived.by(() => {
		if (dateRange === 'all') return groupedByDate;
		const days = parseInt(dateRange);
		const cutoff = new Date();
		cutoff.setDate(cutoff.getDate() - days);
		const cutoffStr = cutoff.toISOString().split('T')[0];
		return groupedByDate.filter(([date]) => date >= cutoffStr);
	});

	const visibleTimeline = $derived(filteredByDate.slice(0, visibleDays));

	// The daily view used to render EVERY story as a full card in raw display_order:
	// 120 news plus up to 462 regulatory filings for a single day (2026-08-14), with the
	// filings interleaved. The front page never had this problem because it ranks and
	// takes 15. Split the two, rank the news, and collapse the filings into a digest.
	const dailyStories = $derived(selectedBriefing?.stories ?? []);
	const dailyNews = $derived([...dailyStories].filter(s => !isFiling(s)).sort(byRankDesc));
	const dailyFilings = $derived(dailyStories.filter(isFiling));

	// Filings are only meaningful grouped by their issuing body (SEC EDGAR 4, FEC, ...).
	const filingsBySource = $derived.by(() => {
		const groups = new Map<string, Story[]>();
		for (const f of dailyFilings) {
			const key = f.source_name || 'Other';
			const bucket = groups.get(key);
			if (bucket) bucket.push(f);
			else groups.set(key, [f]);
		}
		return [...groups.entries()].sort((a, b) => b[1].length - a[1].length);
	});

	let expandedFilingSource = $state<string | null>(null);

	const freedomDefs = [
		{ key: 'time_stories' as const, freedom: 'time', label: 'Time Freedom', color: 'var(--color-freedom-time)' },
		{ key: 'wealth_stories' as const, freedom: 'wealth', label: 'Wealth Freedom', color: 'var(--color-freedom-financial)' },
		{ key: 'location_stories' as const, freedom: 'location', label: 'Location Freedom', color: 'var(--color-freedom-location)' },
		{ key: 'health_stories' as const, freedom: 'health', label: 'Health Freedom', color: 'var(--color-freedom-health)' },
		{ key: 'whoop_stories' as const, freedom: 'whoop', label: 'Whoop', color: 'var(--color-freedom-health)' },
	];

	const rangeOptions = [
		{ value: '7' as const, label: 'Last 7 days' },
		{ value: '30' as const, label: 'Last 30 days' },
		{ value: 'all' as const, label: 'All' },
	];
</script>

<div class="space-y-6 pt-2">
	<div class="flex items-center justify-between">
		<h2 class="text-xl font-semibold text-text">Archive</h2>
		{#if viewMode === 'list'}
			<div class="flex items-center gap-2">
				{#each rangeOptions as opt}
					<button
						class="text-[10px] px-2 py-1 rounded transition-colors {dateRange === opt.value
							? 'bg-bg-card text-text border border-border'
							: 'text-text-muted hover:text-text'}"
						onclick={() => dateRange = opt.value}
					>
						{opt.label}
					</button>
				{/each}
				<span class="text-sm text-text-muted ml-2">{filteredByDate.length} day{filteredByDate.length !== 1 ? 's' : ''}</span>
			</div>
		{/if}
	</div>

	{#if expandedStory}
		<StoryExpanded story={expandedStory} onClose={() => expandedStory = null} />

	{:else if viewMode === 'daily' && selectedBriefing}
		<div class="mb-4">
			<button class="text-sm text-text-muted hover:text-text transition-colors" onclick={backToList}>
				← Back to archive
			</button>
			<div class="flex items-center gap-3 mt-2">
				<h3 class="text-lg font-semibold text-text">
					{formatDate(selectedBriefing.briefing.date)}
				</h3>
				<span class="text-[10px] uppercase tracking-wider bg-ai/10 text-ai px-2 py-0.5 rounded">Daily</span>
			</div>
			<!-- Honest count: the raw story_count folds in hundreds of filings, so a day
			     reading "582 stories" was really 120 stories and 462 filings. -->
			<p class="text-sm text-text-secondary mt-1">
				{dailyNews.length} {dailyNews.length === 1 ? 'story' : 'stories'}{#if dailyFilings.length > 0}<span class="text-text-muted"> · {dailyFilings.length} filings</span>{/if}
			</p>
		</div>

		<!-- Hero comes from the NEWS, never a filing: the old `stories[0]` was whatever
		     landed first in display_order. -->
		{@const hero = dailyNews.find(s => s.is_hero) ?? dailyNews[0]}
		{@const grid = dailyNews.filter(s => s.id !== hero?.id)}

		{#if hero}
			<HeroCard story={hero} onExpand={(s) => expandedStory = s} />
		{/if}
		{#if grid.length > 0}
			<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
				{#each grid as story (story.id)}
					<StoryCard {story} onExpand={(s) => expandedStory = s} />
				{/each}
			</div>
		{/if}

		{#if dailyFilings.length > 0}
			<!-- Regulatory filings: reference material, not reading. Collapsed by source
			     so the day's actual journalism isn't buried under SEC Form 4s. -->
			<section class="mt-10 pt-6 border-t border-border/60">
				<h4 class="text-[10px] uppercase tracking-[0.25em] text-text-muted">
					Filings · {dailyFilings.length} from {filingsBySource.length}
					{filingsBySource.length === 1 ? 'source' : 'sources'}
				</h4>
				<div class="mt-3 space-y-1.5">
					{#each filingsBySource as [source, items] (source)}
						{@const open = expandedFilingSource === source}
						<div class="rounded-lg border border-border/60 overflow-hidden">
							<button
								class="w-full flex items-center justify-between gap-3 px-3 py-2 text-left
									hover:bg-bg-card-hover transition-colors"
								onclick={() => expandedFilingSource = open ? null : source}
								aria-expanded={open}
							>
								<span class="text-xs text-text-secondary truncate">{source}</span>
								<span class="text-[11px] text-text-muted shrink-0 tabular-nums">
									{items.length}
									<span class="inline-block ml-1 transition-transform {open ? 'rotate-90' : ''}">›</span>
								</span>
							</button>
							{#if open}
								<ul class="border-t border-border/60 divide-y divide-border/40 max-h-80 overflow-y-auto">
									{#each items as f (f.id)}
										<li>
											<button
												class="w-full text-left px-3 py-2 text-[11px] leading-relaxed text-text-muted
													hover:text-text hover:bg-bg-card-hover transition-colors"
												onclick={() => expandedStory = f}
											>
												{f.headline}
											</button>
										</li>
									{/each}
								</ul>
							{/if}
						</div>
					{/each}
				</div>
			</section>
		{/if}

	{:else if viewMode === 'freedoms' && selectedFreedoms}
		<div class="mb-4">
			<button class="text-sm text-text-muted hover:text-text transition-colors" onclick={backToList}>
				← Back to archive
			</button>
		</div>
		<div class="max-w-2xl mx-auto pb-12">
			<header class="mb-10">
				<p class="text-[10px] uppercase tracking-[0.3em] text-gold-muted mb-3">{formatDate(selectedFreedoms.date)}</p>
				<h3 class="text-xl font-light text-text tracking-wide mb-1">The Four Freedoms</h3>
				<div class="w-10 h-px bg-gold mt-4 opacity-60"></div>
			</header>
			<div class="space-y-12">
				{#each freedomDefs as f}
					{@const stories = selectedFreedoms[f.key]}
					{#if stories.length > 0}
						<FreedomSection freedom={f.freedom} label={f.label} color={f.color} {stories} />
					{/if}
				{/each}
			</div>
		</div>

	{:else if loading}
		<div class="flex items-center justify-center h-48">
			<div class="w-6 h-6 border-2 border-ai border-t-transparent rounded-full animate-spin"></div>
		</div>

	{:else if error}
		<div class="flex items-center justify-center h-48">
			<div class="text-center max-w-md">
				<div class="text-4xl mb-4">⚠</div>
				<h3 class="text-lg font-semibold text-text mb-2">Error Loading Archive</h3>
				<p class="text-text-secondary text-sm mb-4">{error}</p>
				<button class="text-sm text-ai hover:underline" onclick={loadArchive}>Retry</button>
			</div>
		</div>

	{:else if filteredByDate.length > 0}
		<!-- Magazine timeline: newest first, one issue block per day on a vertical rail -->
		<div class="relative ml-2 border-l border-border/60 pl-7 space-y-8 pb-4">
			{#each visibleTimeline as [date, group], i (date)}
				{@const latestDaily = group.dailies[0]}
				{@const newMonth = i === 0 || monthLabel(date) !== monthLabel(visibleTimeline[i - 1][0])}

				{#if newMonth}
					<div class="relative pt-1 {i === 0 ? '' : 'mt-10'}">
						<span class="absolute -left-[33px] top-1.5 w-3 h-3 rounded-full border-2 border-border bg-bg"></span>
						<p class="text-[10px] uppercase tracking-[0.25em] text-text-muted">{monthLabel(date)}</p>
					</div>
				{/if}

				<section class="relative">
					<span
						class="absolute -left-[31px] top-2 w-2.5 h-2.5 rounded-full {isToday(date) ? 'bg-ai' : 'bg-border'}"
					></span>

					<!-- Day header row -->
					<div class="flex items-baseline justify-between gap-3">
						<div class="flex items-baseline gap-2 min-w-0">
							<h3 class="text-sm font-semibold text-text whitespace-nowrap">{formatDayShort(date)}</h3>
							{#if isToday(date)}
								<span class="text-[10px] uppercase tracking-wider bg-ai/10 text-ai px-1.5 py-0.5 rounded">Today</span>
							{/if}
							{#if latestDaily}
								<span class="text-[11px] text-text-muted whitespace-nowrap">{latestDaily.story_count} stories</span>
							{/if}
						</div>
						<div class="flex items-center gap-1.5 shrink-0">
							{#if group.dailies.length > 1}
								{#each group.dailies as daily}
									<button
										class="text-[10px] px-2 py-1 rounded-md border border-border hover:bg-bg-card-hover
											transition-colors text-text-secondary hover:text-text"
										onclick={() => selectDaily(daily.id)}
									>
										{daily.time_label ?? 'Daily'} · {daily.story_count}
									</button>
								{/each}
							{/if}
							{#if group.freedoms}
								<button
									class="text-[10px] px-2 py-1 rounded-md border transition-colors hover:opacity-90"
									style="border-color: var(--color-gold-dim); color: var(--color-gold); background: var(--color-gold-dim)"
									onclick={() => selectFreedoms(date)}
								>
									Freedoms · {group.freedoms.story_count}
								</button>
							{/if}
						</div>
					</div>

					<!-- Issue block: hero + summary + sectors, whole block opens the day -->
					{#if latestDaily}
						<button
							class="w-full text-left mt-2 rounded-xl border border-border bg-bg-card hover:border-border-hover
								hover:bg-bg-card-hover transition-colors p-4 group"
							onclick={() => selectDaily(latestDaily.id)}
						>
							{#if latestDaily.hero_headline}
								<p class="text-base font-medium text-text leading-snug group-hover:text-ai transition-colors">
									{latestDaily.hero_headline}
								</p>
							{/if}
							{#if latestDaily.executive_summary}
								<p class="text-xs text-text-muted mt-2 leading-relaxed line-clamp-2">
									{truncateSentences(latestDaily.executive_summary, 2)}
								</p>
							{/if}
							<div class="flex gap-2 mt-3">
								{#each ['ai', 'miami', 'italy', 'tech'] as sector}
									{@const count = sector === 'ai' ? latestDaily.ai_count : sector === 'miami' ? latestDaily.miami_count : sector === 'italy' ? latestDaily.italy_count : latestDaily.tech_count}
									{#if count > 0}
										<span
											class="text-[10px] font-mono px-1.5 py-0.5 rounded inline-flex items-center gap-1"
											style="color: {SECTORS[sector as SectorId].color}; background: {SECTORS[sector as SectorId].dimColor}"
										>
											{sectorNames[sector]} {count}
										</span>
									{/if}
								{/each}
							</div>
						</button>
					{/if}
				</section>
			{/each}

			{#if visibleDays < filteredByDate.length}
				<!-- Auto-reveals via IntersectionObserver; the button is the always-works fallback. -->
				<button
					bind:this={sentinel}
					class="w-full h-10 flex items-center justify-center text-[11px] text-text-muted hover:text-text transition-colors"
					onclick={() => visibleDays += PAGE_SIZE}
				>
					Show more · {filteredByDate.length - visibleDays} day{filteredByDate.length - visibleDays !== 1 ? 's' : ''} remaining
				</button>
			{/if}
		</div>
	{:else}
		<div class="flex items-center justify-center h-48">
			<div class="text-center">
				<div class="text-4xl mb-4">◫</div>
				<h3 class="text-lg font-semibold text-text mb-2">No Archives Yet</h3>
				<p class="text-text-secondary text-sm">Past briefings will appear here after your first fetch.</p>
			</div>
		</div>
	{/if}
</div>
