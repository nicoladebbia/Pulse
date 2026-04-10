<script lang="ts">
	import type { ProjectIdea, IdeaStreamEvent } from '$lib/tauri/types';
	import { isTauri, mockIdeas, simulateIdeaGeneration } from '$lib/tauri/mock';
	import { generateIdeas, getIdeas, updateIdeaStatus } from '$lib/tauri/commands';

	let ideas = $state<ProjectIdea[]>([]);
	let isLoading = $state(true);
	let isGenerating = $state(false);
	let progressStage = $state('');
	let progressDetail = $state('');
	let error = $state<string | null>(null);
	let expandedId = $state<number | null>(null);
	let loaded = $state(false);

	$effect(() => {
		if (loaded) return;
		loaded = true;
		loadIdeas();
	});

	async function loadIdeas() {
		error = null;
		try {
			if (!isTauri()) {
				ideas = mockIdeas;
				return;
			}
			const ipc = (window as any).__TAURI_INTERNALS__;
			const result = await ipc.invoke('get_ideas', { statusFilter: null });
			ideas = result;
		} catch (e: any) {
			error = String(e?.message ?? e);
		} finally {
			isLoading = false;
		}
	}

	async function handleGenerate() {
		isGenerating = true;
		progressStage = '';
		progressDetail = '';
		error = null;

		const onEvent = (event: IdeaStreamEvent) => {
			if (event.event === 'Progress') {
				progressStage = event.data.stage;
				progressDetail = event.data.detail;
			} else if (event.event === 'Complete') {
				ideas = [...event.data.ideas, ...ideas];
				isGenerating = false;
			} else if (event.event === 'Error') {
				error = event.data.message;
				isGenerating = false;
			}
		};

		try {
			if (!isTauri()) {
				simulateIdeaGeneration(onEvent);
			} else {
				await generateIdeas(onEvent);
			}
		} catch (e: any) {
			error = String(e?.message ?? e);
			isGenerating = false;
		}
	}

	async function setStatus(id: number, status: string) {
		try {
			if (isTauri()) {
				await updateIdeaStatus(id, status);
			}
			if (status === 'dismissed') {
				ideas = ideas.filter(i => i.id !== id);
			} else {
				ideas = ideas.map(i => i.id === id ? { ...i, status: status as any } : i);
			}
		} catch (e: any) {
			console.error('Failed to update idea status:', e);
		}
	}

	function toggleExpand(id: number) {
		expandedId = expandedId === id ? null : id;
	}

	const difficultyColors: Record<string, string> = {
		weekend: 'text-emerald-400 bg-emerald-400/10',
		week: 'text-amber-400 bg-amber-400/10',
		month: 'text-rose-400 bg-rose-400/10',
	};

	const statusIcons: Record<string, string> = {
		new: '~',
		saved: '*',
		building: '>',
	};
</script>

<div class="max-w-3xl mx-auto py-4 px-4">
	<div class="flex items-center justify-between mb-6">
		<div>
			<h1 class="text-xl font-semibold text-text">Ideas</h1>
			<p class="text-sm text-text-secondary mt-1">Project opportunities from your news</p>
		</div>
		<button
			class="px-4 py-2 text-sm font-medium rounded-lg transition-colors
				{isGenerating ? 'bg-bg-card border border-border text-text-muted cursor-wait' : 'bg-ai/15 text-ai border border-ai/30 hover:bg-ai/25'}"
			disabled={isGenerating}
			onclick={handleGenerate}
		>
			{isGenerating ? 'Generating...' : 'Generate Ideas'}
		</button>
	</div>

	{#if isGenerating}
		<div class="bg-bg-card border border-border rounded-xl p-4 mb-6">
			<div class="flex items-center gap-3">
				<div class="w-2 h-2 bg-ai rounded-full animate-pulse"></div>
				<div>
					<p class="text-sm text-text">{progressDetail || 'Starting...'}</p>
				</div>
			</div>
		</div>
	{/if}

	{#if error}
		<div class="bg-rose-400/10 border border-rose-400/30 rounded-xl p-4 mb-6">
			<p class="text-sm text-rose-400">{error}</p>
		</div>
	{/if}

	{#if isLoading}
		<div class="text-center py-16 text-text-muted text-sm">Loading ideas...</div>
	{:else if ideas.length === 0 && !isGenerating}
		<div class="text-center py-16">
			<div class="text-4xl mb-4">~</div>
			<p class="text-text-secondary text-sm mb-2">No project ideas yet</p>
			<p class="text-text-muted text-xs">Click "Generate Ideas" to analyze your news and discover opportunities</p>
		</div>
	{:else}
		<div class="space-y-3">
			{#each ideas as idea (idea.id)}
				<div class="bg-bg-card border border-border rounded-xl overflow-hidden transition-colors hover:border-border-hover">
					<!-- Card header -->
					<button class="w-full text-left p-4" onclick={() => toggleExpand(idea.id)}>
						<div class="flex items-start justify-between gap-3">
							<div class="flex-1 min-w-0">
								<div class="flex items-center gap-2 mb-1.5">
									<span class="text-xs font-mono text-text-muted">{statusIcons[idea.status] ?? '~'}</span>
									<h3 class="text-sm font-medium text-text truncate">{idea.title}</h3>
								</div>
								<p class="text-xs text-text-secondary line-clamp-2">{idea.description}</p>
							</div>
							<div class="flex items-center gap-2 shrink-0">
								<span class="text-xs px-2 py-0.5 rounded-full {difficultyColors[idea.difficulty] ?? 'text-text-muted bg-bg'}">
									{idea.difficulty}
								</span>
								<span class="text-xs text-text-muted">{expandedId === idea.id ? '-' : '+'}</span>
							</div>
						</div>
					</button>

					<!-- Expanded content -->
					{#if expandedId === idea.id}
						<div class="px-4 pb-4 border-t border-border pt-3 space-y-4">
							<div>
								<h4 class="text-[11px] font-semibold text-text-muted uppercase tracking-wider mb-1.5">Why now</h4>
								<p class="text-[13px] text-text-secondary leading-relaxed">{idea.why_now}</p>
							</div>

							<div>
								<h4 class="text-[11px] font-semibold text-text-muted uppercase tracking-wider mb-1.5">Differentiation</h4>
								<p class="text-[13px] text-text-secondary leading-relaxed">{idea.differentiation}</p>
							</div>

							<div>
								<h4 class="text-[11px] font-semibold text-text-muted uppercase tracking-wider mb-1.5">Tech Stack</h4>
								<p class="text-[13px] text-text-secondary leading-relaxed">{idea.tech_stack}</p>
							</div>

							{#if idea.competitors.length > 0}
								<div>
									<h4 class="text-[11px] font-semibold text-text-muted uppercase tracking-wider mb-1.5">Competitors</h4>
									<div class="space-y-2">
										{#each idea.competitors as comp}
											<div class="bg-bg border border-border rounded-lg p-2.5">
												<div class="text-[13px] text-text font-medium mb-0.5">{comp.name}</div>
												<p class="text-[12px] text-text-secondary leading-relaxed">{comp.weakness}</p>
											</div>
										{/each}
									</div>
								</div>
							{/if}

							<div class="flex items-center gap-2 pt-2 border-t border-border">
								{#if idea.status !== 'saved'}
									<button
										class="text-xs px-3 py-1.5 rounded-lg bg-ai/10 text-ai border border-ai/20 hover:bg-ai/20 transition-colors"
										onclick={() => setStatus(idea.id, 'saved')}
									>
										Save
									</button>
								{/if}
								{#if idea.status !== 'building'}
									<button
										class="text-xs px-3 py-1.5 rounded-lg bg-emerald-400/10 text-emerald-400 border border-emerald-400/20 hover:bg-emerald-400/20 transition-colors"
										onclick={() => setStatus(idea.id, 'building')}
									>
										Start Building
									</button>
								{/if}
								<button
									class="text-xs px-3 py-1.5 rounded-lg bg-rose-400/10 text-rose-400 border border-rose-400/20 hover:bg-rose-400/20 transition-colors"
									onclick={() => setStatus(idea.id, 'dismissed')}
								>
									Dismiss
								</button>
								<div class="flex-1"></div>
								<span class="text-xs text-text-muted">
									Relevance: {Math.round(idea.relevance_score * 100)}%
								</span>
							</div>
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>
