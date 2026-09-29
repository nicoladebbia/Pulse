<script lang="ts">
	import { goto } from '$app/navigation';
	import { SECTORS, type SectorId } from '$lib/config';
	import { isTauri, mockTrends } from '$lib/tauri/mock';
	import { getTrends } from '$lib/tauri/commands';
	import { expandedStoryId } from '$lib/stores/briefing';
	import type { TrendThread } from '$lib/tauri/types';
	import { trackCitationClick } from '$lib/engagement';
	import PredictionsPanel from '$lib/components/predictions/PredictionsPanel.svelte';
	import { getTrendDossier } from '$lib/tauri/commands';
	import { mockTrendDossier } from '$lib/tauri/mock';
	import type { TrendDossier } from '$lib/tauri/types';
	import { relativeDayLabel } from '$lib/date';

	let threads = $state<TrendThread[]>([]);
	let isLoading = $state(true);
	let loaded = $state(false);
	let error = $state<string | null>(null);
	let sectorFilter = $state<string>('all');
	let sortBy = $state<'hottest' | 'fastest' | 'connected'>('hottest');
	let refreshing = $state(false);

	// Predictions used to be its own route. It is a view of the same intelligence the
	// radar shows — the radar's own cards already link to it — so it lives here as a
	// tab instead of a ninth sidebar entry.
	let view = $state<'radar' | 'predictions'>('radar');

	// Dossiers are fetched on first expand and kept, so collapsing and re-expanding
	// costs nothing. Keyed by thread id: two trends can share a title only if the
	// dedup in trends.rs failed, and even then they must not share a cache slot.
	let expandedTrendId = $state<number | null>(null);
	let dossiers = $state<Record<number, TrendDossier>>({});
	let dossierLoading = $state<number | null>(null);
	let dossierError = $state<Record<number, string>>({});

	async function toggleDossier(id: number, topic: string) {
		if (expandedTrendId === id) {
			expandedTrendId = null;
			return;
		}
		expandedTrendId = id;
		if (dossiers[id]) return;

		dossierLoading = id;
		try {
			const d = isTauri() ? await getTrendDossier(topic) : mockTrendDossier(topic);
			dossiers = { ...dossiers, [id]: d };
			dossierError = Object.fromEntries(Object.entries(dossierError).filter(([k]) => Number(k) !== id));
		} catch (e: any) {
			// A failed dossier collapses to a retryable message inside the card. It must
			// never reject uncaught — an unhandled rejection takes the webview down.
			dossierError = { ...dossierError, [id]: String(e?.message ?? e) };
		} finally {
			dossierLoading = null;
		}
	}

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadTrends();
	});

	// When stale signals get recomputed in the background (see get_trends in
	// trends.rs), the backend emits 'trends-recomputed' — re-fetch silently so
	// the page opens instantly on cached data but still ends up fresh.
	$effect(() => {
		if (!isTauri()) return;
		let cancelled = false;
		let unlisten: (() => void) | null = null;
		(async () => {
			// The IIFE is floating, so a rejection from either await escapes as an
			// unhandled rejection and takes the webview with it. The identical block in
			// signals/+page.svelte has this try for the same reason.
			try {
				const { listen } = await import('@tauri-apps/api/event');
				if (cancelled) return;
				const un = await listen('trends-recomputed', async () => {
					refreshing = true;
					try {
						threads = await getTrends();
					} catch { /* keep showing current data */ }
					refreshing = false;
				});
				if (cancelled) { un(); return; }
				unlisten = un;
			} catch {
				// No live refresh, but the page still shows what it loaded. Stale trends
				// beat a crashed page.
			}
		})();
		return () => { cancelled = true; if (unlisten) unlisten(); };
	});

	async function loadTrends() {
		error = null;
		isLoading = true;
		try {
			if (!isTauri()) {
				threads = mockTrends;
				return;
			}
			threads = await getTrends();
		} catch (e: any) {
			error = String(e?.message ?? e);
		} finally {
			isLoading = false;
		}
	}

	const sorted = $derived.by(() => {
		const base = sectorFilter === 'all'
			? threads
			: threads.filter(t => t.sector === sectorFilter);

		if (sortBy === 'fastest') {
			// Acceleration = (w7/7)/(w30/30). When all mentions are recent (w7==w30) it collapses
			// to exactly 30/7 ≈ 4.286 — a huge tie-pile of ~1,700 signals. Sorting on acceleration
			// alone floats that noise cluster above genuinely accelerating entities. Tiebreak by
			// volume (mention_count * days_active) so real momentum wins within the tie.
			return [...base].sort((a, b) =>
				b.acceleration - a.acceleration ||
				(b.mention_count * b.days_active) - (a.mention_count * a.days_active)
			);
		}
		if (sortBy === 'connected') {
			return [...base].sort((a, b) => b.related_entities.length - a.related_entities.length);
		}
		// hottest = default order from backend
		return base;
	});

	const sectors = ['all', 'ai', 'miami', 'italy', 'tech'] as const;
	const sectorLabels: Record<string, string> = {
		all: 'All',
		ai: 'AI & LLMs',
		miami: 'Miami',
		italy: 'Italy',
		tech: 'Tech',
	};

	const trajectoryMeta: Record<string, { icon: string; label: string; class: string }> = {
		dominant: { icon: '◉', label: 'Dominant', class: 'bg-ai/15 text-ai' },
		hot: { icon: '▲', label: 'Hot', class: 'bg-amber-500/15 text-amber-400' },
		rising: { icon: '↗', label: 'Rising', class: 'bg-emerald-500/15 text-emerald-400' },
		fading: { icon: '↘', label: 'Fading', class: 'bg-red-500/15 text-red-400' },
	};

	const sortOptions = [
		{ value: 'hottest' as const, label: 'Hottest' },
		{ value: 'fastest' as const, label: 'Rising Fastest' },
		{ value: 'connected' as const, label: 'Most Connected' },
	];

	function navigateToStory(storyId: number) {
		// Trends spans months, so most of these are outside today's briefing; the front
		// page loads them by id. See +page.svelte.
		trackCitationClick('/trends', storyId);
		expandedStoryId.set(storyId);
		goto('/');
	}

	function askAbout(entity: string) {
		sessionStorage.setItem('pulse_ask_prefill', `Tell me about ${entity}`);
		goto('/ask');
	}

	const formatDate = relativeDayLabel;

	function sentimentColor(val: number): string {
		if (val > 0.2) return 'text-emerald-400';
		if (val < -0.2) return 'text-rose-400';
		return 'text-text-muted';
	}

	function sentimentIcon(val: number): string {
		if (val > 0.2) return '▲';
		if (val < -0.2) return '▼';
		return '─';
	}
</script>

<div class="space-y-4 pt-2">
	<!-- Header -->
	<div class="flex items-center justify-between">
		<div>
			<h2 class="text-xl font-semibold text-text">
				{view === 'radar' ? 'Trend Radar' : 'Predictions'}
			</h2>
			<p class="text-xs text-text-muted mt-0.5">
				{view === 'radar'
					? 'Entities gaining momentum across your archive'
					: 'AI-generated forecasts tracked against reality'}
			</p>
		</div>
		{#if refreshing && view === 'radar'}
			<span class="text-[10px] text-text-muted animate-pulse">updating signals…</span>
		{/if}
	</div>

	<!-- View switch -->
	<div class="inline-flex rounded-lg border border-border p-0.5">
		{#each [{ v: 'radar' as const, label: 'Radar' }, { v: 'predictions' as const, label: 'Predictions' }] as tab}
			<button
				class="text-xs px-3 py-1.5 rounded-md transition-colors {view === tab.v
					? 'bg-bg-card-hover text-text'
					: 'text-text-secondary hover:text-text'}"
				aria-pressed={view === tab.v}
				onclick={() => (view = tab.v)}
			>
				{tab.label}
			</button>
		{/each}
	</div>

{#if view === 'predictions'}
	<PredictionsPanel />
{:else}

	<!-- Sector filters + Sort -->
	<div class="flex items-center justify-between gap-3">
		<div class="flex gap-1.5">
			{#each sectors as s}
				{@const isActive = sectorFilter === s}
				<button
					class="text-xs px-3 py-1.5 rounded-lg transition-colors {isActive
						? 'bg-ai/15 text-ai border border-ai/30'
						: 'bg-bg-card border border-border text-text-muted hover:text-text hover:bg-bg-card-hover'}"
					onclick={() => sectorFilter = s}
				>
					{sectorLabels[s]}
					{#if s !== 'all'}
						{@const count = threads.filter(t => t.sector === s).length}
						{#if count > 0}
							<span class="ml-1 text-[10px] opacity-60">{count}</span>
						{/if}
					{/if}
				</button>
			{/each}
		</div>
		<div class="flex gap-1">
			{#each sortOptions as opt}
				<button
					class="text-[10px] px-2 py-1 rounded transition-colors {sortBy === opt.value
						? 'bg-bg-card text-text border border-border'
						: 'text-text-muted hover:text-text'}"
					onclick={() => sortBy = opt.value}
				>
					{opt.label}
				</button>
			{/each}
		</div>
	</div>

	{#if isLoading}
		<!-- Skeleton loading -->
		<div class="space-y-3">
			{#each Array(5) as _}
				<div class="bg-bg-card border border-border rounded-xl p-4 animate-pulse">
					<div class="flex items-center gap-3">
						<div class="w-24 h-4 bg-border rounded"></div>
						<div class="w-16 h-4 bg-border rounded"></div>
					</div>
					<div class="mt-3 space-y-2">
						<div class="w-full h-3 bg-border rounded"></div>
						<div class="w-3/4 h-3 bg-border rounded"></div>
					</div>
				</div>
			{/each}
		</div>
	{:else if error}
		<div class="flex items-center justify-center h-64">
			<div class="text-center max-w-md">
				<div class="text-4xl mb-4">⚠</div>
				<h3 class="text-lg font-semibold text-text mb-2">Error Loading Trends</h3>
				<p class="text-text-secondary text-sm mb-4">{error}</p>
				<button class="text-sm text-ai hover:underline" onclick={loadTrends}>Retry</button>
			</div>
		</div>
	{:else if sorted.length > 0}
		<div class="space-y-3">
			{#each sorted as thread (thread.id)}
				{@const color = SECTORS[thread.sector as SectorId]?.color ?? 'var(--color-text-muted)'}
				{@const traj = trajectoryMeta[thread.trajectory] ?? trajectoryMeta.rising}
				{@const maxSpark = Math.max(...thread.sparkline, 1)}
				{@const isCrossSector = thread.sectors.length >= 2}

				<div class="bg-bg-card border border-border rounded-xl overflow-hidden">
					<!-- Card header -->
					<div class="px-4 pt-3 pb-2 border-l-2" style="border-left-color: {color}">
						<div class="flex items-center justify-between">
							<div class="flex items-center gap-2 flex-wrap">
								<button
									class="text-sm font-semibold text-text hover:text-ai transition-colors"
									onclick={() => askAbout(thread.title)}
									title="Ask Pulse about {thread.title}"
								>
									{thread.title}
								</button>
								<span class="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium {traj.class}">
									{traj.icon} {traj.label}
								</span>

								<!-- Acceleration badge -->
								{#if thread.acceleration >= 1.5}
									<span class="text-[10px] px-1.5 py-0.5 rounded bg-violet-500/15 text-violet-400 font-mono">
										{thread.acceleration.toFixed(1)}x
									</span>
								{/if}

								<!-- Sentiment indicator -->
								<span class="text-[10px] {sentimentColor(thread.sentiment_avg)}" title="Sentiment: {thread.sentiment_avg.toFixed(2)}">
									{sentimentIcon(thread.sentiment_avg)}
								</span>

								<!-- Cross-sector badge -->
								{#if isCrossSector}
									<span class="text-[10px] px-1.5 py-0.5 rounded bg-cyan-500/15 text-cyan-400" title="Appears in: {thread.sectors.join(', ')}">
										⊕ {thread.sectors.length} sectors
									</span>
								{/if}
							</div>
							<div class="flex items-center gap-3 text-[10px] text-text-muted shrink-0">
								<span>{thread.mention_count} mentions</span>
								<span>{thread.days_active}d active</span>
							</div>
						</div>

						<!-- Sparkline -->
						<div class="flex items-end gap-px mt-2 h-4">
							{#each thread.sparkline as value, i}
								{@const height = value > 0 ? Math.max(3, (value / maxSpark) * 16) : 0}
								<div
									class="flex-1 rounded-sm transition-all"
									style="height: {height}px; background: {value > 0 ? color : 'transparent'}; opacity: {0.3 + (i / 14) * 0.7}"
									title="{value} mention{value !== 1 ? 's' : ''}"
								></div>
							{/each}
						</div>

						<!-- Related entities pills -->
						{#if thread.related_entities.length > 0}
							<div class="flex items-center gap-1.5 mt-2 flex-wrap">
								<span class="text-[9px] text-text-muted uppercase tracking-wider">Related:</span>
								{#each thread.related_entities.slice(0, 4) as rel}
									<button
										class="text-[10px] px-1.5 py-0.5 rounded bg-bg border border-border text-text-secondary hover:text-text hover:border-ai/30 transition-colors"
										onclick={() => askAbout(rel.name)}
										title="Co-mentioned {rel.strength} times"
									>
										{rel.name}
									</button>
								{/each}
							</div>
						{/if}

						<!-- Causal consequence -->
						{#if thread.causal_consequence}
							<div class="mt-2 flex items-center gap-1.5">
								<span class="text-[9px] text-text-muted">→</span>
								<span class="text-[10px] text-amber-400/80 italic">Expect: {thread.causal_consequence}</span>
							</div>
						{/if}

						<!-- Linked prediction -->
						{#if thread.prediction}
							<div class="mt-2 flex items-center gap-1.5">
								<span class="text-[9px] text-violet-400">⊕</span>
								<button
									class="text-[10px] text-violet-400/80 hover:text-violet-400 transition-colors text-left"
									onclick={() => (view = 'predictions')}
								>
									Prediction: {thread.prediction.title} ({Math.round(thread.prediction.confidence * 100)}%)
								</button>
							</div>
						{/if}
					</div>

					<!-- Story timeline -->
					<div class="px-4 pb-3 pt-1">
						<div class="space-y-1">
							{#each thread.points as point}
								<button
									class="w-full flex items-center gap-2.5 text-left py-1 px-2 -mx-2 rounded-lg
										hover:bg-bg-card-hover transition-colors group"
									onclick={() => navigateToStory(point.story_id)}
								>
									<span class="text-[10px] text-text-muted shrink-0 w-16">{formatDate(point.date)}</span>
									<span class="w-1 h-1 rounded-full shrink-0" style="background: {color}"></span>
									<span class="text-xs text-text-secondary group-hover:text-text transition-colors truncate">
										{point.headline}
									</span>
								</button>
							{/each}
						</div>

						<!-- Dossier toggle. The card shows 8 story points; the dossier is the
						     full 30-day window plus every prediction and the wider entity set. -->
						<button
							class="mt-2 w-full flex items-center justify-center gap-1.5 py-1.5 rounded-lg text-[10px]
								text-text-muted hover:text-text hover:bg-bg-card-hover transition-colors"
							aria-expanded={expandedTrendId === thread.id}
							onclick={() => toggleDossier(thread.id, thread.title)}
						>
							<span class="transition-transform {expandedTrendId === thread.id ? 'rotate-90' : ''}">›</span>
							{expandedTrendId === thread.id ? 'Hide dossier' : 'Open dossier'}
						</button>
					</div>

					{#if expandedTrendId === thread.id}
						<div class="border-t border-border bg-bg/40 px-4 py-3 space-y-4">
							{#if dossierLoading === thread.id}
								<p class="text-[11px] text-text-muted animate-pulse">Loading dossier…</p>
							{:else if dossierError[thread.id]}
								<div class="flex items-center gap-2">
									<p class="text-[11px] text-rose-400">{dossierError[thread.id]}</p>
									<button
										class="text-[10px] text-ai hover:underline"
										onclick={() => { expandedTrendId = null; toggleDossier(thread.id, thread.title); }}
									>
										Retry
									</button>
								</div>
							{:else if dossiers[thread.id]}
								{@const d = dossiers[thread.id]}

								<!-- Full story timeline -->
								<div>
									<p class="text-[10px] uppercase tracking-wider text-text-muted mb-1.5">
										Coverage · {d.stories.length} {d.stories.length === 1 ? 'story' : 'stories'} in 30 days
									</p>
									{#if d.stories.length > 0}
										<div class="space-y-1">
											{#each d.stories as story (story.story_id)}
												<button
													class="w-full flex items-start gap-2.5 text-left py-1 px-2 -mx-2 rounded-lg
														hover:bg-bg-card-hover transition-colors group"
													onclick={() => navigateToStory(story.story_id)}
												>
													<span class="text-[10px] text-text-muted shrink-0 w-16 pt-0.5">{formatDate(story.date)}</span>
													<span class="w-1 h-1 rounded-full shrink-0 mt-1.5" style="background: {SECTORS[story.sector as SectorId]?.color ?? color}"></span>
													<span class="min-w-0">
														<span class="block text-xs text-text-secondary group-hover:text-text transition-colors">
															{story.headline}
														</span>
														{#if story.what_to_watch}
															<span class="block text-[10px] text-amber-400/70 italic mt-0.5">
																Watch: {story.what_to_watch}
															</span>
														{/if}
													</span>
												</button>
											{/each}
										</div>
									{:else}
										<p class="text-[11px] text-text-muted">No stories in the last 30 days.</p>
									{/if}
								</div>

								<!-- Every prediction on this topic, not just the active one on the card -->
								{#if d.predictions.length > 0}
									<div>
										<p class="text-[10px] uppercase tracking-wider text-text-muted mb-1.5">Predictions</p>
										<div class="space-y-1">
											{#each d.predictions as p}
												<div class="flex items-baseline gap-2">
													<span class="text-[10px] font-mono text-violet-400 shrink-0">{Math.round(p.confidence * 100)}%</span>
													<span class="text-xs text-text-secondary">{p.title}</span>
													<span class="text-[10px] text-text-muted shrink-0">{p.status}</span>
												</div>
											{/each}
										</div>
									</div>
								{/if}

								{#if d.related_entities.length > 0}
									<div>
										<p class="text-[10px] uppercase tracking-wider text-text-muted mb-1.5">Related</p>
										<div class="flex gap-1.5 flex-wrap">
											{#each d.related_entities as rel}
												<button
													class="text-[10px] px-1.5 py-0.5 rounded bg-bg border border-border text-text-secondary
														hover:text-text hover:border-ai/30 transition-colors"
													onclick={() => askAbout(rel.name)}
													title="Co-mentioned {rel.strength} times"
												>
													{rel.name}
												</button>
											{/each}
										</div>
									</div>
								{/if}

								<button
									class="text-[11px] text-ai hover:underline"
									onclick={() => askAbout(thread.title)}
								>
									Ask Pulse about {thread.title} →
								</button>
							{/if}
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{:else}
		<div class="flex items-center justify-center h-64">
			<div class="text-center max-w-md">
				<div class="text-4xl mb-4">◈</div>
				<h3 class="text-lg font-semibold text-text mb-2">
					{#if sectorFilter !== 'all'}
						No trends in {sectorLabels[sectorFilter]}
					{:else}
						Building Your Trend Radar
					{/if}
				</h3>
				<p class="text-text-secondary text-sm leading-relaxed">
					{#if sectorFilter !== 'all'}
						Try selecting "All" to see trends across all sectors.
					{:else}
						Trends appear after a few days of briefings as Pulse detects entities
						that keep showing up across your news archive.
					{/if}
				</p>
			</div>
		</div>
	{/if}
{/if}
</div>
