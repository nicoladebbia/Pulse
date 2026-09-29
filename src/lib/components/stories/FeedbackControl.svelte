<script lang="ts">
	import { feedbackLabels, requestFeedback, toggleFeedback, type FeedbackLabel } from '$lib/stores/feedback';

	// `labelled`: words, always visible (the expanded view). Otherwise two small glyphs
	// that appear on hover of the enclosing `group/fb` card — and stay once a label is
	// set, so a recorded judgment is visible at a glance.
	let { storyId, labelled = false }: { storyId: number; labelled?: boolean } = $props();

	$effect(() => {
		requestFeedback(storyId);
	});

	let current = $derived($feedbackLabels[storyId] ?? null);

	function tap(e: MouseEvent, label: FeedbackLabel) {
		// Cards are clickable; a feedback tap must not also open the story.
		e.stopPropagation();
		void toggleFeedback(storyId, label);
	}
</script>

<div
	class="inline-flex items-center gap-1 transition-opacity
		{labelled ? '' : 'rounded bg-bg-card-hover'}
		{labelled || current ? '' : 'opacity-0 group-hover/fb:opacity-100 focus-within:opacity-100'}"
	role="group"
	aria-label="Did this story matter to you?"
>
	<button
		type="button"
		class="rounded px-1.5 py-0.5 text-[11px] leading-none transition-colors
			{current === 'mattered' ? 'text-emerald-400 bg-emerald-500/10' : 'text-text-muted hover:text-text'}"
		aria-pressed={current === 'mattered'}
		title="Mattered"
		onclick={(e) => tap(e, 'mattered')}
	>
		{labelled ? 'Mattered' : '✓'}
	</button>
	<button
		type="button"
		class="rounded px-1.5 py-0.5 text-[11px] leading-none transition-colors
			{current === 'didnt' ? 'text-text-secondary bg-bg-card-hover' : 'text-text-muted hover:text-text'}"
		aria-pressed={current === 'didnt'}
		title="Didn't matter"
		onclick={(e) => tap(e, 'didnt')}
	>
		{labelled ? "Didn't" : '✕'}
	</button>
</div>
