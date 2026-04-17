import { writable, get } from 'svelte/store';
import type { ChatThread, ChatMessage, ConversationResponse, ProactiveInsight, ChatStreamEvent, SearchStep } from '$lib/tauri/types';
import { chatSendStream } from '$lib/tauri/commands';
import { isTauri, simulateChatStream } from '$lib/tauri/mock';

// Panel state
export const panelOpen = writable(false);
export const activeThreadId = writable<string | null>(null);
export const threads = writable<ChatThread[]>([]);
export const messages = writable<ChatMessage[]>([]);
export const isStreaming = writable(false);
export const isSearching = writable(false);
export const searchSteps = writable<SearchStep[]>([]);
export const lastResponse = writable<ConversationResponse | null>(null);
export const lastSearchSource = writable<string>('archive');

// Cancel flag — when set, streaming stops rendering + signals backend to stop sending deltas
let cancelRequested = false;
export function cancelStream() {
	cancelRequested = true;
	// Signal backend to stop sending delta events
	if (isTauri()) {
		const ipc = (window as any).__TAURI_INTERNALS__;
		ipc?.invoke('chat_cancel_stream').catch(() => {});
	}
}

// Regenerate — re-send the last user message
export function regenerateLastMessage() {
	const msgs = get(messages);
	// Find last user message
	const lastUserMsg = [...msgs].reverse().find(m => m.role === 'user');
	if (!lastUserMsg || get(isStreaming)) return;

	// Remove the last assistant message
	const lastAssistant = [...msgs].reverse().find(m => m.role === 'assistant');
	if (lastAssistant) {
		messages.update(m => m.filter(msg => msg.id !== lastAssistant.id));
	}

	// Re-send
	sendMessage(lastUserMsg.content);
}

// Generation counter to prevent stale stream updates after thread switch
let streamGeneration = 0;

// Topic colors for thread display
export const TOPIC_COLORS: Record<string, string> = {
	ai: 'var(--color-ai)',
	miami: 'var(--color-miami)',
	italy: 'var(--color-italy)',
	tech: 'var(--color-tech)',
	general: 'var(--color-text-secondary)',
};

export const TOPIC_LABELS: Record<string, string> = {
	ai: 'AI & LLMs',
	miami: 'Miami Beach',
	italy: 'Italy',
	tech: 'Tech & Innovation',
	general: 'General',
};

export function togglePanel() {
	panelOpen.update(v => !v);
}

export async function loadThreads() {
	try {
		const ipc = (window as any).__TAURI_INTERNALS__;
		if (!ipc) return;
		const result = await ipc.invoke('chat_list_threads');
		threads.set(result);
	} catch (e) {
		console.error('Failed to load threads:', e);
	}
}

export async function loadThread(threadId: string) {
	streamGeneration++; // Invalidate any in-flight stream
	activeThreadId.set(threadId);
	try {
		const ipc = (window as any).__TAURI_INTERNALS__;
		if (!ipc) return;
		const result = await ipc.invoke('chat_get_thread', { threadId });
		messages.set(result);
	} catch (e) {
		console.error('Failed to load thread:', e);
	}
}

export async function sendMessage(message: string) {
	if (!message.trim() || get(isStreaming)) return;

	const threadId = get(activeThreadId);
	isStreaming.set(true);
	isSearching.set(true);
	searchSteps.set([]);
	cancelRequested = false;

	// Add optimistic user message
	const tempMsg: ChatMessage = {
		id: crypto.randomUUID(),
		thread_id: threadId ?? '',
		role: 'user',
		content: message,
		sources: null,
		metadata: null,
		created_at: new Date().toISOString(),
	};
	messages.update(m => [...m, tempMsg]);

	// Add placeholder assistant message that will be updated as tokens stream in
	const assistantMsgId = crypto.randomUUID();
	const assistantMsg: ChatMessage = {
		id: assistantMsgId,
		thread_id: threadId ?? '',
		role: 'assistant',
		content: '',
		sources: null,
		metadata: null,
		created_at: new Date().toISOString(),
	};
	messages.update(m => [...m, assistantMsg]);

	let accumulated = '';
	const myGeneration = ++streamGeneration;

	try {
		const streamFn = isTauri()
			? (cb: (event: ChatStreamEvent) => void) => chatSendStream(threadId, message, cb)
			: (cb: (event: any) => void) => simulateChatStream(cb);

		await streamFn((event: ChatStreamEvent) => {
			// Discard events if user switched threads during this stream
			if (streamGeneration !== myGeneration) return;

			if (event.event === 'Searching') {
				searchSteps.update(steps => {
					// Mark previous steps as done
					const updated = steps.map(s => ({ ...s, done: true }));
					updated.push({ stage: event.data.stage, detail: event.data.detail, done: false });
					return updated;
				});
				return;
			}

			if (event.event === 'Delta') {
				// Cancel requested — stop rendering but let backend finish
				if (cancelRequested) return;

				// First delta means search is done, streaming has started
				if (get(isSearching)) {
					isSearching.set(false);
					searchSteps.update(steps => steps.map(s => ({ ...s, done: true })));
				}
				accumulated += event.data.text;
				// Update the assistant message in-place with streaming text
				messages.update(m => {
					const updated = [...m];
					const last = updated.find(msg => msg.id === assistantMsgId);
					if (last) {
						last.content = accumulated;
					}
					return updated;
				});
			} else if (event.event === 'Complete') {
				// Replace raw streamed text with cleaned version + update sources + real message ID
				messages.update(m => {
					const updated = [...m];
					const last = updated.find(msg => msg.id === assistantMsgId);
					if (last) {
						last.id = event.data.message_id; // Replace frontend UUID with backend ID for feedback
						last.content = event.data.message;
						last.sources = event.data.source_story_ids;
						last.metadata = {
							estimated_cost: event.data.estimated_cost,
							model_used: event.data.model_used,
							search_source: event.data.search_source,
						};
					}
					return updated;
				});
				lastResponse.set({
					message: accumulated,
					source_story_ids: event.data.source_story_ids,
					suggested_followups: event.data.suggested_followups,
					thread_topic: event.data.thread_topic,
					thread_title: event.data.thread_title,
					proactive_connections: event.data.proactive_connections,
				});
				if ('search_source' in event.data) {
					lastSearchSource.set(event.data.search_source);
				}
			} else if (event.event === 'Error') {
				messages.update(m => {
					const updated = [...m];
					const last = updated.find(msg => msg.id === assistantMsgId);
					if (last) {
						last.content = accumulated + `\n\nError: ${event.data.message}`;
					}
					return updated;
				});
			}
		});

		// If this was a new thread, update the active thread
		if (!threadId) {
			await loadThreads();
			const currentThreads = get(threads);
			const newThread = currentThreads[0];
			if (newThread) {
				activeThreadId.set(newThread.id);
			}
		}
	} catch (e: any) {
		// If streaming failed entirely, update the placeholder
		messages.update(m => {
			const updated = [...m];
			const last = updated.find(msg => msg.id === assistantMsgId);
			if (last) {
				last.content = accumulated || `Error: ${String(e?.message ?? e)}`;
			}
			return updated;
		});
	} finally {
		isStreaming.set(false);
		isSearching.set(false);
	}
}

export async function deleteThread(threadId: string) {
	try {
		const ipc = (window as any).__TAURI_INTERNALS__;
		if (!ipc) return;
		await ipc.invoke('chat_delete_thread', { threadId });
		threads.update(t => t.filter(th => th.id !== threadId));
		if (get(activeThreadId) === threadId) {
			activeThreadId.set(null);
			messages.set([]);
		}
	} catch (e) {
		console.error('Failed to delete thread:', e);
	}
}

export function startNewThread() {
	activeThreadId.set(null);
	messages.set([]);
	lastResponse.set(null);
}

export function formatTimestamp(isoDate: string): string {
	const date = new Date(isoDate);
	const now = new Date();
	const diffMs = now.getTime() - date.getTime();
	const diffMins = Math.floor(diffMs / 60000);

	if (diffMins < 1) return 'Just now';
	if (diffMins < 60) return `${diffMins}m ago`;

	const diffHours = Math.floor(diffMins / 60);
	if (diffHours < 24) return `${diffHours}h ago`;

	return date.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
}
