<script lang="ts">
	import type { ChatMessage } from '$lib/tauri/types';
	import ChatSources from './ChatSources.svelte';

	let { message }: { message: ChatMessage } = $props();

	function renderMarkdown(text: string): string {
		const lines = text.split('\n');
		const result: string[] = [];
		let inCodeBlock = false;
		let codeLines: string[] = [];
		let codeLang = '';

		for (const line of lines) {
			const trimmed = line.trim();

			// Code block toggle
			if (trimmed.startsWith('```')) {
				if (!inCodeBlock) {
					inCodeBlock = true;
					codeLang = trimmed.slice(3).trim();
					codeLines = [];
				} else {
					inCodeBlock = false;
					result.push(`<pre class="bg-bg-expanded border border-border rounded-lg p-3 my-2 overflow-x-auto"><code class="text-xs font-mono text-text-secondary">${codeLines.map(l => esc(l)).join('\n')}</code></pre>`);
				}
				continue;
			}
			if (inCodeBlock) { codeLines.push(line); continue; }

			// Headers
			if (trimmed.startsWith('### '))
				{ result.push(`<h4 class="text-xs font-semibold uppercase tracking-wider text-text-muted mt-3 mb-1">${esc(trimmed.slice(4))}</h4>`); continue; }
			if (trimmed.startsWith('## '))
				{ result.push(`<h3 class="text-sm font-semibold text-text mt-3 mb-1.5">${inline(trimmed.slice(3))}</h3>`); continue; }
			if (trimmed.startsWith('# '))
				{ result.push(`<h2 class="text-base font-semibold text-text mt-3 mb-1.5">${inline(trimmed.slice(2))}</h2>`); continue; }

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

			// Empty line = paragraph break
			if (trimmed === '') { result.push('<div class="h-2"></div>'); continue; }

			// Regular paragraph
			result.push(`<p>${inline(trimmed)}</p>`);
		}

		return result.join('\n');
	}

	function esc(s: string): string {
		return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
			.replace(/"/g, '&quot;').replace(/'/g, '&#39;');
	}

	function inline(s: string): string {
		// Extract links before escaping
		const links: [string, string][] = [];
		let linkIdx = 0;
		let pre = s.replace(/\[([^\]]+)\]\((https?:\/\/[^)]+)\)/g, (_, text, url) => {
			links.push([text, url]);
			return `__LINK_${linkIdx++}__`;
		});
		let out = esc(pre);
		// Bold: **text**
		out = out.replace(/\*\*(.+?)\*\*/g, '<strong class="font-semibold text-text">$1</strong>');
		// Bold: __text__ (but not our link placeholders)
		out = out.replace(/__(?!LINK_)(.+?)__/g, '<strong class="font-semibold text-text">$1</strong>');
		// Italic: *text*
		out = out.replace(/\*(.+?)\*/g, '<em class="italic text-text-secondary">$1</em>');
		// Inline code: `text`
		out = out.replace(/`(.+?)`/g, '<code class="text-xs bg-bg-card-hover px-1 py-0.5 rounded font-mono">$1</code>');
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
		<div class="bg-bg-card border border-border rounded-2xl rounded-bl-sm px-3.5 py-2.5 max-w-[90%] text-sm text-text-secondary leading-relaxed chat-content">
			{@html renderMarkdown(message.content)}
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
	.chat-content :global(h2),
	.chat-content :global(h3),
	.chat-content :global(h4) {
		margin: 0;
	}
	.chat-content :global(strong) {
		color: var(--color-text);
	}
</style>
