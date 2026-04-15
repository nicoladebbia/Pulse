<script lang="ts">
	import type { ChatMessage } from '$lib/tauri/types';
	import ChatSources from './ChatSources.svelte';

	import { regenerateLastMessage } from '$lib/stores/chat';
	import ChatFollowups from './ChatFollowups.svelte';

	let { message, isLast = false, followups = [], searchSource = '', onAsk = (_s: string) => {} }: {
		message: ChatMessage;
		isLast?: boolean;
		followups?: string[];
		searchSource?: string;
		onAsk?: (s: string) => void;
	} = $props();

	// Feedback state
	let feedbackRating = $state<'up' | 'down' | null>(null);
	let showReasonInput = $state(false);
	let feedbackReason = $state('');
	let feedbackSubmitted = $state(false);

	// Copy state
	let copied = $state(false);

	async function copyResponse() {
		try {
			await navigator.clipboard.writeText(message.content);
			copied = true;
			setTimeout(() => { copied = false; }, 2000);
		} catch (e) {
			console.error('Copy failed:', e);
		}
	}

	async function submitFeedback(rating: 'up' | 'down') {
		feedbackRating = rating;
		if (rating === 'down') {
			showReasonInput = true;
			return;
		}
		await sendFeedback(rating, null);
	}

	async function sendFeedback(rating: 'up' | 'down', reason: string | null) {
		try {
			const ipc = (window as any).__TAURI_INTERNALS__;
			if (!ipc) return;
			await ipc.invoke('submit_chat_feedback', {
				messageId: message.id,
				threadId: message.thread_id,
				rating,
				reason,
			});
			feedbackSubmitted = true;
			showReasonInput = false;
		} catch (e) {
			console.error('Failed to submit feedback:', e);
		}
	}

	function submitReason() {
		sendFeedback('down', feedbackReason.trim() || null);
	}

	function skipReason() {
		sendFeedback('down', null);
	}

	interface ParsedSection {
		type: 'bluf' | 'analysis' | 'watch' | 'text';
		content: string;
	}

	// Detect if the response has any meaningful structure (## headers, --- dividers, or our expected format)
	const hasStructure = $derived(
		message.role === 'assistant' &&
		message.content.length > 200 &&
		(/^##\s+/m.test(message.content) || /^---$/m.test(message.content))
	);

	const sections = $derived.by((): ParsedSection[] => {
		if (!hasStructure) return [{ type: 'text', content: message.content }];

		const lines = message.content.split('\n');
		const parts: ParsedSection[] = [];
		let currentType: ParsedSection['type'] = 'bluf'; // First section before any header = BLUF
		let currentLines: string[] = [];
		let seenFirstHeader = false;

		for (const line of lines) {
			const trimmed = line.trim();
			let newType: ParsedSection['type'] | null = null;

			// Exact matches for our expected format
			if (/^##\s+Bottom Line/i.test(trimmed)) newType = 'bluf';
			else if (/^##\s+Analysis/i.test(trimmed)) newType = 'analysis';
			else if (/^##\s+(What to Watch|Watch|Looking Ahead|Outlook)/i.test(trimmed)) newType = 'watch';
			// Any other ## header → analysis section
			else if (/^##\s+/.test(trimmed)) {
				if (!seenFirstHeader) {
					// First header after opening text = start of analysis
					newType = 'analysis';
				} else {
					// Subsequent ## headers stay in current section — renderMarkdown handles them
					currentLines.push(line);
					continue;
				}
			}
			// --- divider → treat as section break, next content is analysis
			else if (trimmed === '---') {
				if (currentLines.length > 0) {
					const content = currentLines.join('\n').trim();
					if (content) parts.push({ type: currentType, content });
				}
				if (!seenFirstHeader) {
					currentType = 'analysis';
					seenFirstHeader = true;
				}
				currentLines = [];
				continue;
			}

			if (newType) {
				seenFirstHeader = true;
				if (currentLines.length > 0) {
					const content = currentLines.join('\n').trim();
					if (content) parts.push({ type: currentType, content });
				}
				currentType = newType;
				currentLines = [];
			} else {
				currentLines.push(line);
			}
		}
		if (currentLines.length > 0) {
			const content = currentLines.join('\n').trim();
			if (content) parts.push({ type: currentType, content });
		}

		// If the last section looks like a "what to watch" but wasn't detected, check for keywords
		if (parts.length > 2) {
			const last = parts[parts.length - 1];
			if (last.type === 'analysis' && /\b(watch|monitor|track|keep an eye)\b/i.test(last.content) && last.content.length < 500) {
				last.type = 'watch';
			}
		}

		return parts;
	});

	function renderMarkdown(text: string): string {
		const lines = text.split('\n');
		const result: string[] = [];
		let inCodeBlock = false;
		let codeLines: string[] = [];

		for (const line of lines) {
			const trimmed = line.trim();

			if (trimmed.startsWith('```')) {
				if (!inCodeBlock) {
					inCodeBlock = true;
					codeLines = [];
				} else {
					inCodeBlock = false;
					result.push(`<pre class="bg-bg-expanded border border-border rounded-lg p-3 my-2 overflow-x-auto"><code class="text-xs font-mono text-text-secondary">${codeLines.map(l => esc(l)).join('\n')}</code></pre>`);
				}
				continue;
			}
			if (inCodeBlock) { codeLines.push(line); continue; }

			// Headers
			if (trimmed.startsWith('## '))
				{ result.push(`<h3 class="text-sm font-semibold text-text mt-4 mb-1.5">${esc(trimmed.slice(3))}</h3>`); continue; }
			if (trimmed.startsWith('### '))
				{ result.push(`<h4 class="text-xs font-semibold uppercase tracking-wider text-text-muted mt-3 mb-1">${esc(trimmed.slice(4))}</h4>`); continue; }

			// Bullet points (- or *)
			if (trimmed.startsWith('- ') || trimmed.startsWith('* '))
				{ result.push(`<div class="flex items-start gap-2 ml-1"><span class="text-ai mt-0.5 shrink-0 text-xs">▪</span><span>${inline(trimmed.slice(2))}</span></div>`); continue; }

			// Numbered lists
			if (/^\d+\.\s/.test(trimmed)) {
				const num = trimmed.match(/^(\d+)\.\s/)![1];
				const content = trimmed.replace(/^\d+\.\s/, '');
				result.push(`<div class="flex items-start gap-2 ml-1"><span class="text-text-muted mt-0.5 shrink-0 text-xs font-mono">${num}.</span><span>${inline(content)}</span></div>`);
				continue;
			}

			if (trimmed === '') { result.push('<div class="h-2"></div>'); continue; }

			result.push(`<p>${inline(trimmed)}</p>`);
		}

		return result.join('\n');
	}

	function esc(s: string): string {
		return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
			.replace(/"/g, '&quot;').replace(/'/g, '&#39;');
	}

	function inline(s: string): string {
		const links: [string, string][] = [];
		let linkIdx = 0;
		let pre = s.replace(/\[([^\]]+)\]\((https?:\/\/[^)]+)\)/g, (_, text, url) => {
			links.push([text, url]);
			return `__LINK_${linkIdx++}__`;
		});
		let out = esc(pre);
		// Bold
		out = out.replace(/\*\*(.+?)\*\*/g, '<strong class="font-semibold text-text">$1</strong>');
		out = out.replace(/__(?!LINK_)(.+?)__/g, '<strong class="font-semibold text-text">$1</strong>');
		// Italic
		out = out.replace(/\*(.+?)\*/g, '<em class="italic text-text-secondary">$1</em>');
		// Inline code
		out = out.replace(/`(.+?)`/g, '<code class="text-xs bg-bg-card-hover px-1 py-0.5 rounded font-mono">$1</code>');
		// Confidence badges — match "HIGH CONFIDENCE", "High confidence", "HIGH:", "high", etc.
		out = out.replace(/\b(HIGH)\s*(CONFIDENCE)?:?/gi, '<span class="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium bg-emerald-500/15 text-emerald-400">HIGH</span>');
		out = out.replace(/\b(MODERATE)\s*(CONFIDENCE)?:?/gi, '<span class="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium bg-amber-500/15 text-amber-400">MODERATE</span>');
		out = out.replace(/\b(LOW)\s*(CONFIDENCE)?:?/gi, '<span class="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium bg-red-500/15 text-red-400">LOW</span>');
		// Restore links
		for (let i = 0; i < links.length; i++) {
			const [text, url] = links[i];
			out = out.replace(`__LINK_${i}__`, `<a href="${esc(url)}" target="_blank" rel="noopener" class="text-ai hover:underline">${esc(text)}</a>`);
		}
		return out;
	}
</script>

{#if message.role === 'user'}
	<div class="flex justify-end">
		<div class="bg-ai text-white rounded-2xl rounded-br-sm px-3.5 py-2.5 max-w-[85%] text-sm leading-relaxed">
			{message.content}
		</div>
	</div>
{:else}
	<div class="flex justify-start">
		<div class="max-w-[90%] text-sm text-text-secondary leading-relaxed space-y-2">
			{#if hasStructure}
				<!-- Structured intelligence response -->
				{#each sections as section}
					{#if section.type === 'bluf'}
						<div class="bg-ai/8 border-l-2 border-ai rounded-r-xl px-3.5 py-2.5">
							<div class="text-[10px] font-semibold uppercase tracking-wider text-ai mb-1">Bottom Line</div>
							<div class="text-text leading-relaxed chat-content">
								{@html renderMarkdown(section.content)}
							</div>
						</div>
					{:else if section.type === 'watch'}
						<div class="bg-bg-card border border-border rounded-xl px-3.5 py-2.5">
							<div class="text-[10px] font-semibold uppercase tracking-wider text-text-muted mb-1">What to Watch</div>
							<div class="chat-content">
								{@html renderMarkdown(section.content)}
							</div>
						</div>
					{:else}
						<div class="bg-bg-card border border-border rounded-2xl rounded-bl-sm px-3.5 py-2.5 chat-content">
							{@html renderMarkdown(section.content)}
						</div>
					{/if}
				{/each}
			{:else}
				<!-- Plain text response (during streaming or unstructured) -->
				<div class="bg-bg-card border border-border rounded-2xl rounded-bl-sm px-3.5 py-2.5 chat-content">
					{@html renderMarkdown(message.content)}
				</div>
			{/if}

			<!-- Follow-ups (inside message, before sources) -->
			{#if isLast && followups.length > 0}
				<ChatFollowups {followups} onAsk={onAsk} />
			{/if}

			{#if message.sources?.length}
				<ChatSources storyIds={message.sources} />
			{/if}

			<!-- Search source badge -->
			{#if isLast && searchSource && searchSource !== 'archive'}
				<div class="text-xs text-text-muted mt-1 flex items-center gap-1.5">
					{#if searchSource === 'web'}
						<span class="w-1.5 h-1.5 rounded-full bg-amber-400"></span> Answered from web search
					{:else if searchSource === 'archive+web'}
						<span class="w-1.5 h-1.5 rounded-full bg-emerald-400"></span> Answered from archive + web search
					{/if}
				</div>
			{/if}

			<!-- Action row (assistant messages only) -->
			{#if message.role === 'assistant' && message.content.length > 0}
				<div class="flex items-center gap-1 mt-1.5">
					<!-- Copy -->
					<button
						onclick={copyResponse}
						class="p-1.5 rounded hover:bg-bg-card-hover transition-colors {copied ? 'text-emerald-400' : 'text-text-muted hover:text-text-secondary'}"
						title="Copy response"
					>
						{#if copied}
							<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" class="w-3.5 h-3.5"><path d="M3 8.5L6.5 12L13 4" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>
						{:else}
							<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" class="w-3.5 h-3.5"><rect x="5" y="5" width="8" height="8" rx="1.5" stroke-width="1.5"/><path d="M3 11V3.5A1.5 1.5 0 0 1 4.5 2H11" stroke-width="1.5" stroke-linecap="round"/></svg>
						{/if}
					</button>
					<!-- Regenerate (last message only) -->
					{#if isLast}
						<button
							onclick={regenerateLastMessage}
							class="p-1.5 rounded hover:bg-bg-card-hover transition-colors text-text-muted hover:text-text-secondary"
							title="Regenerate response"
						>
							<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" class="w-3.5 h-3.5"><path d="M2 8a6 6 0 0 1 10.2-4.3M14 8a6 6 0 0 1-10.2 4.3" stroke-width="1.5" stroke-linecap="round"/><path d="M12.5 1.5v3h-3M3.5 14.5v-3h3" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>
						</button>
					{/if}
					<!-- Divider -->
					<span class="w-px h-3 bg-border mx-0.5"></span>
					<!-- Feedback -->
					{#if feedbackSubmitted}
						<span class="text-[10px] text-text-muted">Thanks</span>
					{:else}
						<button
							onclick={() => submitFeedback('up')}
							disabled={feedbackRating !== null}
							class="p-1.5 rounded hover:bg-bg-card-hover transition-colors {feedbackRating === 'up' ? 'text-emerald-400' : 'text-text-muted hover:text-text-secondary'}"
							title="Good response"
						>
							<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" class="w-4 h-4">
								<path d="M1 8.25a1.25 1.25 0 1 1 2.5 0v7.5a1.25 1.25 0 1 1-2.5 0v-7.5ZM5.5 6v9.25a2.3 2.3 0 0 0 .67 1.62l.23.24a2.5 2.5 0 0 0 1.57.66l4.53.23a2.5 2.5 0 0 0 2.47-1.77l1.78-5.34A1.25 1.25 0 0 0 15.56 9H11.5V4.75A2.25 2.25 0 0 0 9.25 2.5H9a.75.75 0 0 0-.72.54L6.08 10H5.5V6Z"/>
							</svg>
						</button>
						<button
							onclick={() => submitFeedback('down')}
							disabled={feedbackRating !== null}
							class="p-1.5 rounded hover:bg-bg-card-hover transition-colors {feedbackRating === 'down' ? 'text-red-400' : 'text-text-muted hover:text-text-secondary'}"
							title="Poor response"
						>
							<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" class="w-4 h-4">
								<path d="M19 11.75a1.25 1.25 0 1 1-2.5 0v-7.5a1.25 1.25 0 1 1 2.5 0v7.5ZM14.5 14V4.75a2.3 2.3 0 0 0-.67-1.62l-.23-.24A2.5 2.5 0 0 0 12.03 2.23l-4.53-.23A2.5 2.5 0 0 0 5.03 3.77L3.25 9.11A1.25 1.25 0 0 0 4.44 11H8.5v4.25A2.25 2.25 0 0 0 10.75 17.5h.25a.75.75 0 0 0 .72-.54L13.92 10H14.5V14Z"/>
							</svg>
						</button>
					{/if}
					{#if message.metadata?.estimated_cost}
						<span class="text-[10px] text-text-muted font-mono ml-auto">
							{message.metadata.model_used ?? ''} · ${(message.metadata.estimated_cost as number).toFixed(3)}
						</span>
					{/if}
				</div>
				{#if showReasonInput}
					<div class="flex items-center gap-1.5 mt-1">
						<input
							type="text"
							bind:value={feedbackReason}
							placeholder="What was wrong? (optional)"
							class="flex-1 text-xs bg-bg-expanded border border-border rounded-lg px-2.5 py-1.5 text-text placeholder:text-text-muted focus:outline-none focus:border-ai/50"
							onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter') submitReason(); }}
						/>
						<button
							onclick={submitReason}
							class="text-[10px] text-ai hover:text-ai/80 font-medium px-2 py-1"
						>Send</button>
						<button
							onclick={skipReason}
							class="text-[10px] text-text-muted hover:text-text-secondary px-1 py-1"
						>Skip</button>
					</div>
				{/if}
			{/if}
		</div>
	</div>
{/if}

<style>
	.chat-content :global(p) {
		margin: 0;
	}
	.chat-content :global(h3),
	.chat-content :global(h4) {
		margin: 0;
	}
	.chat-content :global(strong) {
		color: var(--color-text);
	}
</style>
