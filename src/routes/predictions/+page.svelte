<script lang="ts">
	import { isTauri, mockPredictions, mockPredictionStats } from '$lib/tauri/mock';
	import { getAllPredictions, getPredictionStats, manuallyValidatePrediction } from '$lib/tauri/commands';
	import type { Prediction, PredictionStats } from '$lib/tauri/types';

	let predictions = $state<Prediction[]>([]);
	let stats = $state<PredictionStats | null>(null);
	let isLoading = $state(true);
	let error = $state<string | null>(null);
	let expandedId = $state<number | null>(null);
	let statusFilter = $state<string>('all');
	let loaded = $state(false);

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadData();
	});

	async function loadData() {
		error = null;
		isLoading = true;
		try {
			if (!isTauri()) {
				predictions = mockPredictions;
				stats = mockPredictionStats;
				return;
			}
			const [p, s] = await Promise.all([getAllPredictions(), getPredictionStats()]);
			predictions = p;
			stats = s;
		} catch (e: any) {
			error = String(e?.message ?? e);
		} finally {
			isLoading = false;
		}
	}

	async function handleValidate(id: number, outcome: string) {
		try {
			if (isTauri()) {
				await manuallyValidatePrediction(id, outcome);
			}
			predictions = predictions.map(p =>
				p.id === id ? { ...p, status: outcome as Prediction['status'] } : p
			);
			// Refresh stats
			if (isTauri()) {
				stats = await getPredictionStats();
			}
		} catch (e: any) {
			console.error('Failed to validate prediction:', e);
		}
	}

	const filtered = $derived(
		statusFilter === 'all'
			? predictions
			: predictions.filter(p => p.status === statusFilter)
	);

	const expiringSoon = $derived(
		predictions.filter(p => {
			if (p.status !== 'active' || !p.predicted_timeframe) return false;
			const target = new Date(p.predicted_timeframe + 'T00:00:00');
			const now = new Date();
			const daysLeft = Math.ceil((target.getTime() - now.getTime()) / 86400000);
			return daysLeft > 0 && daysLeft <= 30;
		})
	);

	function daysUntil(dateStr: string): number {
		const target = new Date(dateStr + 'T00:00:00');
		const now = new Date();
		return Math.ceil((target.getTime() - now.getTime()) / 86400000);
	}

	function formatDate(dateStr: string): string {
		if (!dateStr) return '—';
		const d = new Date(dateStr + 'T12:00:00');
		return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric' });
	}

	const statusMeta: Record<string, { icon: string; label: string; class: string }> = {
		active: { icon: '◉', label: 'Active', class: 'bg-blue-500/15 text-blue-400' },
		validated: { icon: '✓', label: 'Validated', class: 'bg-emerald-500/15 text-emerald-400' },
		partially_validated: { icon: '~', label: 'Partial', class: 'bg-amber-500/15 text-amber-400' },
		invalidated: { icon: '✗', label: 'Wrong', class: 'bg-rose-500/15 text-rose-400' },
		expired: { icon: '○', label: 'Expired', class: 'bg-zinc-500/15 text-zinc-400' },
	};

	const filterOptions = [
		{ value: 'all', label: 'All' },
		{ value: 'active', label: 'Active' },
		{ value: 'validated', label: 'Validated' },
		{ value: 'invalidated', label: 'Wrong' },
		{ value: 'expired', label: 'Expired' },
	];
</script>

<div class="space-y-4 pt-2">
	<!-- Header -->
	<div>
		<h2 class="text-xl font-semibold text-text">Predictions</h2>
		<p class="text-xs text-text-muted mt-0.5">AI-generated forecasts tracked against reality</p>
	</div>

	<!-- Stats Card -->
	{#if stats && stats.total > 0}
		<div class="bg-bg-card border border-border rounded-xl p-4">
			<div class="grid grid-cols-5 gap-4">
				<div class="text-center">
					<div class="text-lg font-semibold text-text">{stats.active}</div>
					<div class="text-[10px] text-text-muted uppercase tracking-wider">Active</div>
				</div>
				<div class="text-center">
					<div class="text-lg font-semibold text-emerald-400">{stats.validated}</div>
					<div class="text-[10px] text-text-muted uppercase tracking-wider">Right</div>
				</div>
				<div class="text-center">
					<div class="text-lg font-semibold text-amber-400">{stats.partially_validated}</div>
					<div class="text-[10px] text-text-muted uppercase tracking-wider">Partial</div>
				</div>
				<div class="text-center">
					<div class="text-lg font-semibold text-rose-400">{stats.invalidated}</div>
					<div class="text-[10px] text-text-muted uppercase tracking-wider">Wrong</div>
				</div>
				<div class="text-center">
					<div class="text-lg font-semibold text-zinc-400">{stats.expired}</div>
					<div class="text-[10px] text-text-muted uppercase tracking-wider">Expired</div>
				</div>
			</div>

			<!-- Accuracy bar -->
			{#if stats.validated + stats.partially_validated + stats.invalidated + stats.expired > 0}
				<div class="mt-4 pt-3 border-t border-border">
					<div class="flex items-center justify-between mb-1.5">
						<span class="text-xs text-text-muted">Accuracy</span>
						<span class="text-xs font-semibold font-mono text-text">{Math.round(stats.accuracy_rate * 100)}%</span>
					</div>
					<div class="w-full h-1.5 bg-border rounded-full overflow-hidden">
						<div
							class="h-full bg-emerald-400 rounded-full transition-all duration-700"
							style="width: {stats.accuracy_rate * 100}%"
						></div>
					</div>
					{#if stats.avg_brier_score != null}
						<div class="flex items-center justify-between mt-2">
							<span class="text-[10px] text-text-muted">Brier Score</span>
							<span class="text-[10px] font-mono text-text-secondary" title="0 = perfect, 0.25 = random, 1 = always wrong">
								{stats.avg_brier_score.toFixed(3)}
							</span>
						</div>
					{/if}
				</div>
			{/if}
		</div>
	{/if}

	<!-- Expiring Soon -->
	{#if expiringSoon.length > 0}
		<div class="bg-amber-500/5 border border-amber-500/20 rounded-xl p-3">
			<div class="flex items-center gap-2 mb-2">
				<span class="text-amber-400 text-sm">⏳</span>
				<span class="text-xs font-medium text-amber-400 uppercase tracking-wider">Expiring Soon</span>
			</div>
			<div class="space-y-1.5">
				{#each expiringSoon as pred}
					{@const days = daysUntil(pred.predicted_timeframe)}
					<button
						class="w-full flex items-center justify-between text-left py-1 px-2 rounded-lg hover:bg-amber-500/10 transition-colors"
						onclick={() => expandedId = expandedId === pred.id ? null : pred.id}
					>
						<span class="text-xs text-text-secondary truncate mr-2">{pred.title}</span>
						<span class="text-[10px] text-amber-400 font-mono shrink-0">{days}d left</span>
					</button>
				{/each}
			</div>
		</div>
	{/if}

	<!-- Filter tabs -->
	<div class="flex gap-1.5">
		{#each filterOptions as opt}
			{@const isActive = statusFilter === opt.value}
			{@const count = opt.value === 'all' ? predictions.length : predictions.filter(p => p.status === opt.value).length}
			<button
				class="text-xs px-3 py-1.5 rounded-lg transition-colors {isActive
					? 'bg-ai/15 text-ai border border-ai/30'
					: 'bg-bg-card border border-border text-text-muted hover:text-text hover:bg-bg-card-hover'}"
				onclick={() => statusFilter = opt.value}
			>
				{opt.label}
				{#if count > 0}
					<span class="ml-1 text-[10px] opacity-60">{count}</span>
				{/if}
			</button>
		{/each}
	</div>

	<!-- Prediction List -->
	{#if isLoading}
		<div class="space-y-3">
			{#each Array(4) as _}
				<div class="bg-bg-card border border-border rounded-xl p-4 animate-pulse">
					<div class="flex items-center gap-3">
						<div class="w-32 h-4 bg-border rounded"></div>
						<div class="w-16 h-4 bg-border rounded"></div>
					</div>
					<div class="mt-3 w-full h-2 bg-border rounded"></div>
				</div>
			{/each}
		</div>
	{:else if error}
		<div class="flex items-center justify-center h-64">
			<div class="text-center max-w-md">
				<div class="text-4xl mb-4">⚠</div>
				<h3 class="text-lg font-semibold text-text mb-2">Error Loading Predictions</h3>
				<p class="text-text-secondary text-sm mb-4">{error}</p>
				<button class="text-sm text-ai hover:underline" onclick={loadData}>Retry</button>
			</div>
		</div>
	{:else if filtered.length > 0}
		<div class="space-y-3">
			{#each filtered as pred (pred.id)}
				{@const meta = statusMeta[pred.status] ?? statusMeta.active}
				{@const days = pred.predicted_timeframe ? daysUntil(pred.predicted_timeframe) : null}
				{@const confidencePct = Math.round(pred.confidence * 100)}

				<div class="bg-bg-card border border-border rounded-xl overflow-hidden transition-colors hover:border-border-hover">
					<!-- Card header -->
					<button class="w-full text-left p-4" onclick={() => expandedId = expandedId === pred.id ? null : pred.id}>
						<div class="flex items-start justify-between gap-3">
							<div class="flex-1 min-w-0">
								<div class="flex items-center gap-2 mb-1.5">
									<span class="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium {meta.class}">
										{meta.icon} {meta.label}
									</span>
									{#if pred.sector}
										<span class="text-[10px] text-text-muted uppercase">{pred.sector}</span>
									{/if}
									<h3 class="text-sm font-medium text-text truncate">{pred.title}</h3>
								</div>

								<!-- Confidence bar + sparkline -->
								<div class="flex items-center gap-2 mt-2">
									{#if pred.probability_history.length >= 2}
										{@const hist = pred.probability_history}
										{@const maxH = Math.max(...hist)}
										{@const minH = Math.min(...hist)}
										{@const range = maxH - minH || 0.1}
										<div class="flex items-end gap-px h-3 w-12 shrink-0" title="Confidence over time: {hist.map(v => Math.round(v * 100) + '%').join(' → ')}">
											{#each hist as val, i}
												{@const height = Math.max(2, ((val - minH) / range) * 12)}
												{@const isLast = i === hist.length - 1}
												<div
													class="flex-1 rounded-sm {isLast ? 'bg-ai' : 'bg-ai/40'}"
													style="height: {height}px"
												></div>
											{/each}
										</div>
									{/if}
									<div class="flex-1 h-1 bg-border rounded-full overflow-hidden">
										<div
											class="h-full rounded-full transition-all duration-500 {confidencePct >= 70 ? 'bg-emerald-400' : confidencePct >= 50 ? 'bg-amber-400' : 'bg-rose-400'}"
											style="width: {confidencePct}%"
										></div>
									</div>
									<span class="text-[10px] font-mono text-text-muted shrink-0">{confidencePct}%</span>
								</div>
							</div>

							<div class="flex flex-col items-end gap-1 shrink-0">
								{#if days !== null && pred.status === 'active'}
									<span class="text-[10px] font-mono {days <= 7 ? 'text-amber-400' : days <= 30 ? 'text-text-secondary' : 'text-text-muted'}">
										{days > 0 ? `${days}d left` : 'Due'}
									</span>
								{/if}
								<span class="text-xs text-text-muted">{expandedId === pred.id ? '−' : '+'}</span>
							</div>
						</div>
					</button>

					<!-- Expanded content -->
					{#if expandedId === pred.id}
						<div class="px-4 pb-4 border-t border-border pt-3 space-y-3">
							<div>
								<h4 class="text-[11px] font-semibold text-text-muted uppercase tracking-wider mb-1">Prediction</h4>
								<p class="text-[13px] text-text-secondary leading-relaxed">{pred.prediction}</p>
							</div>

							{#if pred.reasoning}
								<div>
									<h4 class="text-[11px] font-semibold text-text-muted uppercase tracking-wider mb-1">Reasoning</h4>
									<p class="text-[13px] text-text-secondary leading-relaxed">{pred.reasoning}</p>
								</div>
							{/if}

							<div class="flex items-center gap-4 text-[11px] text-text-muted">
								{#if pred.predicted_timeframe}
									<span>Target: {formatDate(pred.predicted_timeframe)}</span>
								{/if}
								{#if pred.evidence_types.length > 0}
									<span>Evidence: {pred.evidence_types.join(', ')}</span>
								{/if}
							</div>

							<!-- Manual validation buttons (only for active predictions) -->
							{#if pred.status === 'active'}
								<div class="flex items-center gap-2 pt-2 border-t border-border">
									<span class="text-[10px] text-text-muted mr-1">Resolve:</span>
									<button
										class="text-xs px-3 py-1.5 rounded-lg bg-emerald-400/10 text-emerald-400 border border-emerald-400/20 hover:bg-emerald-400/20 transition-colors"
										onclick={() => handleValidate(pred.id, 'validated')}
									>
										Correct
									</button>
									<button
										class="text-xs px-3 py-1.5 rounded-lg bg-amber-400/10 text-amber-400 border border-amber-400/20 hover:bg-amber-400/20 transition-colors"
										onclick={() => handleValidate(pred.id, 'partially_validated')}
									>
										Partially
									</button>
									<button
										class="text-xs px-3 py-1.5 rounded-lg bg-rose-400/10 text-rose-400 border border-rose-400/20 hover:bg-rose-400/20 transition-colors"
										onclick={() => handleValidate(pred.id, 'invalidated')}
									>
										Wrong
									</button>
								</div>
							{/if}
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{:else}
		<div class="flex items-center justify-center h-64">
			<div class="text-center max-w-md">
				<div class="text-4xl mb-4">◎</div>
				<h3 class="text-lg font-semibold text-text mb-2">
					{#if statusFilter !== 'all'}
						No {statusFilter.replace('_', ' ')} predictions
					{:else}
						No Predictions Yet
					{/if}
				</h3>
				<p class="text-text-secondary text-sm leading-relaxed">
					{#if statusFilter !== 'all'}
						Try "All" to see predictions across all statuses.
					{:else}
						Predictions are generated automatically during daily briefings as Pulse identifies
						forward-looking signals in your news.
					{/if}
				</p>
			</div>
		</div>
	{/if}
</div>
