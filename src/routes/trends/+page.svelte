<script lang="ts">
	import { SECTORS, type SectorId } from '$lib/config';
	import { isTauri, mockTrends } from '$lib/tauri/mock';
	import { getTrends } from '$lib/tauri/commands';
	import type { TrendThread } from '$lib/tauri/types';

	let threads = $state<TrendThread[]>([]);
	let selectedThread = $state<TrendThread | null>(null);
	let isLoading = $state(true);
	let loaded = $state(false);
	let error = $state<string | null>(null);

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadTrends();
	});

	async function loadTrends() {
		error = null;
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

	function selectThread(thread: TrendThread) {
		selectedThread = selectedThread?.id === thread.id ? null : thread;
	}

	// Generate date labels for the last 14 days
	const days = Array.from({ length: 14 }, (_, i) => {
		const d = new Date();
		d.setDate(d.getDate() - (13 - i));
		return d.toISOString().split('T')[0];
	});

	const dayLabels = days.map(d => {
		const date = new Date(d + 'T12:00:00');
		return date.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
	});

	const trajectoryIcons: Record<string, string> = {
		emerging: '↗',
		growing: '⬆',
		peaking: '⬤',
		declining: '↘',
	};
</script>

<div class="space-y-6 pt-2">
	<div class="flex items-center justify-between">
		<h2 class="text-xl font-semibold text-text">Trend Radar</h2>
		<p class="text-sm text-text-muted">Story threads evolving over time</p>
	</div>

	{#if isLoading}
		<div class="flex items-center justify-center h-64">
			<div class="w-6 h-6 border border-ai border-t-transparent rounded-full animate-spin"></div>
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
	{:else if threads.length > 0}
		<!-- Timeline River SVG -->
		<div class="bg-bg-card border border-border rounded-xl p-6 overflow-x-auto">
			<svg width="100%" height={threads.length * 50 + 40} viewBox="0 0 800 {threads.length * 50 + 40}">
				<!-- Date axis -->
				{#each dayLabels as label, i}
					<text
						x={60 + i * 52}
						y="15"
						class="text-[10px]"
						fill="var(--color-text-muted)"
						text-anchor="middle"
					>
						{label}
					</text>
				{/each}

				<!-- Thread rows -->
				{#each threads as thread, ti}
					{@const y = 40 + ti * 50}
					{@const color = SECTORS[thread.sector as SectorId]?.color ?? 'var(--color-text-muted)'}

					<!-- Thread label -->
					<!-- svelte-ignore a11y_click_events_have_key_events -->
					<!-- svelte-ignore a11y_no_static_element_interactions -->
					<text x="0" y={y + 5} fill={color} class="text-[11px] font-medium cursor-pointer" dominant-baseline="middle"
						onclick={() => selectThread(thread)}>
						{trajectoryIcons[thread.trajectory] ?? ''} {thread.title.slice(0, 20)}
					</text>

					<!-- Connection line -->
					{#each thread.points as point, pi}
						{@const dayIdx = days.indexOf(point.date)}
						{#if dayIdx >= 0}
							{@const cx = 60 + dayIdx * 52}
							<!-- Dot -->
							<!-- svelte-ignore a11y_click_events_have_key_events -->
							<circle
								cx={cx}
								cy={y}
								r={3 + point.significance * 0.5}
								fill={color}
								opacity="0.8"
								class="cursor-pointer hover:opacity-100"
								onclick={() => selectThread(thread)}
							>
								<title>{point.headline} ({point.date})</title>
							</circle>
							<!-- Line to next point -->
							{#if pi < thread.points.length - 1}
								{@const nextDayIdx = days.indexOf(thread.points[pi + 1].date)}
								{#if nextDayIdx >= 0}
									<line
										x1={cx}
										y1={y}
										x2={60 + nextDayIdx * 52}
										y2={y}
										stroke={color}
										stroke-width="2"
										opacity="0.4"
									/>
								{/if}
							{/if}
						{/if}
					{/each}
				{/each}
			</svg>
		</div>

		<!-- Thread detail on click -->
		{#if selectedThread}
			<div class="bg-bg-card border border-border rounded-xl p-6">
				<div class="flex items-center justify-between mb-4">
					<h3 class="text-lg font-semibold text-text">{selectedThread.title}</h3>
					<button class="text-sm text-text-muted hover:text-text" onclick={() => selectedThread = null}>×</button>
				</div>
				<div class="space-y-3">
					{#each selectedThread.points as point}
						<div class="flex items-center gap-3 text-sm">
							<span class="text-text-muted shrink-0 w-20">{point.date}</span>
							<div class="w-2 h-2 rounded-full shrink-0" style="background: {SECTORS[selectedThread.sector as SectorId]?.color}"></div>
							<span class="text-text">{point.headline}</span>
						</div>
					{/each}
				</div>
			</div>
		{/if}
	{:else}
		<div class="flex items-center justify-center h-64">
			<div class="text-center max-w-md">
				<div class="text-4xl mb-4">◈</div>
				<h3 class="text-lg font-semibold text-text mb-2">Building Your Trend Radar</h3>
				<p class="text-text-secondary text-sm leading-relaxed">
					After 3+ days of briefings, Pulse will detect story threads that evolve over time and
					display them here as a timeline river. Each thread connects related stories across days.
				</p>
			</div>
		</div>
	{/if}
</div>
