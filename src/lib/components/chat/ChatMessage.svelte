<script lang="ts">
	import type { ChatMessage } from '$lib/tauri/types';
	import ChatSources from './ChatSources.svelte';

	let { message }: { message: ChatMessage } = $props();

	// Detect BLUF structure: ## Bottom Line ... ## Analysis ...
	const hasStructure = $derived(
		message.role === 'assistant' && /^##\s+(Bottom Line|Analysis|What to Watch)/m.test(message.content)
	);

	interface ParsedSection {
		type: 'bluf' | 'analysis' | 'watch' | 'text';
		content: string;
	}

	const sections = $derived.by((): ParsedSection[] => {
		if (!hasStructure) return [{ type: 'text', content: message.content }];

		const parts: ParsedSection[] = [];
		const lines = message.content.split('\n');
		let currentType: ParsedSection['type'] = 'text';
		let currentLines: string[] = [];

		for (const line of lines) {
			const trimmed = line.trim();
			let newType: ParsedSection['type'] | null = null;

			if (/^##\s+Bottom Line/i.test(trimmed)) newType = 'bluf';
			else if (/^##\s+Analysis/i.test(trimmed)) newType = 'analysis';
			else if (/^##\s+What to Watch/i.test(trimmed)) newType = 'watch';

			if (newType) {
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

			// Headers (skip ## since we already parsed sections)
			if (trimmed.startsWith('### '))
				{ result.push(`<h4 class="text-xs font-semibold uppercase tracking-wider text-text-muted mt-3 mb-1">${esc(trimmed.slice(4))}</h4>`); continue; }

			// Bullet points
			if (trimmed.startsWith('- '))
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
		// Confidence badges
		out = out.replace(/\bHIGH CONFIDENCE\b/g, '<span class="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium bg-emerald-500/15 text-emerald-400">HIGH</span>');
		out = out.replace(/\bMODERATE CONFIDENCE\b/g, '<span class="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium bg-amber-500/15 text-amber-400">MODERATE</span>');
		out = out.replace(/\bLOW CONFIDENCE\b/g, '<span class="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-medium bg-red-500/15 text-red-400">LOW</span>');
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

			{#if message.sources?.length}
				<ChatSources storyIds={message.sources} />
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
