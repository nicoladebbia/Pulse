<script lang="ts">
	import type { FreedomStory } from '$lib/tauri/types';

	let { freedom, label, color, stories }: {
		freedom: string;
		label: string;
		color: string;
		stories: FreedomStory[];
	} = $props();

	let heroStory = $derived(stories[0] ?? null);
	let otherStories = $derived(stories.slice(1));
	let expandedId = $state<number | null>(null);

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

	function openSource(url: string) {
		const ipc = (window as any).__TAURI_INTERNALS__;
		if (ipc) ipc.invoke('plugin:shell|open', { path: url });
		else window.open(url, '_blank');
	}
</script>

<section>
	<!-- Section header -->
	<div class="flex items-center gap-3 mb-5">
		<div class="w-1.5 h-1.5 rounded-full" style="background: {color}"></div>
		<h3 class="text-[11px] font-medium uppercase tracking-[0.2em]" style="color: {color}">
			{label}
		</h3>
		<div class="flex-1 h-px" style="background: linear-gradient(to right, color-mix(in srgb, {color} 20%, transparent), transparent)"></div>
	</div>

	<!-- Hero story -->
	{#if heroStory}
		<div class="pl-5 mb-5 border-l transition-colors duration-300"
			style="border-color: {expandedId === heroStory.id ? color : 'var(--color-border-subtle)'}">

			<button class="block w-full text-left" onclick={() => toggleExpand(heroStory.id)}>
				<h4 class="text-base font-normal text-text leading-relaxed mb-2 tracking-tight">
					{heroStory.headline}
				</h4>
				{#if expandedId !== heroStory.id}
					<p class="text-sm text-text-muted leading-relaxed mb-3 max-w-2xl">
						{heroStory.summary.length > 200 ? heroStory.summary.slice(0, 200) + '...' : heroStory.summary}
					</p>
				{/if}
				<div class="flex items-center gap-2 text-[10px] text-text-muted uppercase tracking-wider">
					<span>{heroStory.source_name}</span>
					{#if heroStory.published_at}
						<span class="opacity-40">·</span>
						<span>{timeAgo(heroStory.published_at)}</span>
					{/if}
				</div>
			</button>

			<!-- Expanded report -->
			{#if expandedId === heroStory.id}
				<div class="mt-4 space-y-4 animate-fade-in">
					<p class="text-sm text-text-secondary leading-relaxed">{heroStory.summary}</p>

					{#if heroStory.key_facts.length > 0}
						<div>
							<h5 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-2">Key Facts</h5>
							<ul class="space-y-1.5">
								{#each heroStory.key_facts as fact}
									<li class="flex items-start gap-2 text-sm text-text-secondary">
										<span class="mt-1 shrink-0 opacity-40" style="color: {color}">·</span>
										<span>{fact}</span>
									</li>
								{/each}
							</ul>
						</div>
					{/if}

					{#if heroStory.why_it_matters}
						<div class="pl-4 border-l-2" style="border-color: color-mix(in srgb, {color} 30%, transparent)">
							<h5 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1.5">Why It Matters</h5>
							<p class="text-sm text-text-secondary leading-relaxed">{heroStory.why_it_matters}</p>
						</div>
					{/if}

					{#if heroStory.what_to_watch}
						<div>
							<h5 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1.5">What to Watch</h5>
							<p class="text-sm text-text-secondary leading-relaxed">{heroStory.what_to_watch}</p>
						</div>
					{/if}

					<button
						class="text-[10px] uppercase tracking-wider text-text-muted hover:text-text transition-colors"
						onclick={() => openSource(heroStory.original_url)}
					>
						Read original source →
					</button>
				</div>
			{/if}
		</div>
	{/if}

	<!-- Supporting stories -->
	{#if otherStories.length > 0}
		<div class="pl-5 space-y-1">
			{#each otherStories as story}
				<div>
					<button
						class="block w-full text-left py-2 group/story"
						onclick={() => toggleExpand(story.id)}
					>
						<div class="flex items-start gap-3">
							<span class="text-[6px] mt-2 shrink-0 transition-opacity"
								style="color: {color}; opacity: {expandedId === story.id ? 0.8 : 0.3}">●</span>
							<div>
								<p class="text-sm leading-snug transition-colors duration-200"
									style="color: {expandedId === story.id ? 'var(--color-text)' : 'var(--color-text-secondary)'}">
									{story.headline}
								</p>
								<span class="text-[10px] text-text-muted uppercase tracking-wider">{story.source_name}</span>
							</div>
						</div>
					</button>

					<!-- Expanded report -->
					{#if expandedId === story.id}
						<div class="ml-6 pl-4 border-l border-border-subtle mt-1 mb-3 space-y-3 animate-fade-in">
							<p class="text-sm text-text-secondary leading-relaxed">{story.summary}</p>

							{#if story.key_facts.length > 0}
								<div>
									<h5 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1.5">Key Facts</h5>
									<ul class="space-y-1">
										{#each story.key_facts as fact}
											<li class="flex items-start gap-2 text-sm text-text-secondary">
												<span class="mt-1 shrink-0 opacity-40" style="color: {color}">·</span>
												<span>{fact}</span>
											</li>
										{/each}
									</ul>
								</div>
							{/if}

							{#if story.why_it_matters}
								<div class="pl-3 border-l-2" style="border-color: color-mix(in srgb, {color} 25%, transparent)">
									<h5 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1">Why It Matters</h5>
									<p class="text-sm text-text-secondary leading-relaxed">{story.why_it_matters}</p>
								</div>
							{/if}

							{#if story.what_to_watch}
								<div>
									<h5 class="text-[10px] font-medium uppercase tracking-[0.15em] text-text-muted mb-1">What to Watch</h5>
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

	{#if stories.length === 0}
		<p class="pl-5 text-sm text-text-muted italic">No intelligence gathered today</p>
	{/if}
</section>

<style>
	@keyframes fade-in {
		from { opacity: 0; transform: translateY(-4px); }
		to { opacity: 1; transform: translateY(0); }
	}
	.animate-fade-in {
		animation: fade-in 0.2s ease-out;
	}
</style>
