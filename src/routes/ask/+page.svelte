<script lang="ts">
	// TODO: Ask Pulse RAG chat interface
	let query = $state('');
</script>

<div class="max-w-2xl mx-auto pt-8">
	<div class="text-center mb-8">
		<div class="text-4xl mb-4">◎</div>
		<h2 class="text-xl font-semibold text-text mb-2">Ask Pulse</h2>
		<p class="text-text-secondary">Ask questions about your news archive.</p>
	</div>

	<div class="relative">
		<input
			type="text"
			bind:value={query}
			placeholder="What happened in AI today?"
			class="w-full bg-bg-card border border-border rounded-lg px-4 py-3 text-text
				placeholder:text-text-muted focus:outline-none focus:border-ai transition-colors"
		/>
	</div>
</div>
