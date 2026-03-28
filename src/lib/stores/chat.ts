import { writable } from 'svelte/store';

export interface ChatMessage {
	id: string;
	role: 'user' | 'assistant';
	content: string;
	sources?: number[];
	timestamp: Date;
}

export const messages = writable<ChatMessage[]>([]);
export const isStreaming = writable(false);
export const sessionId = writable(crypto.randomUUID());

export function addUserMessage(content: string) {
	const msg: ChatMessage = {
		id: crypto.randomUUID(),
		role: 'user',
		content,
		timestamp: new Date(),
	};
	messages.update(m => [...m, msg]);
	return msg;
}

export function addAssistantMessage(content: string, sources?: number[]) {
	const msg: ChatMessage = {
		id: crypto.randomUUID(),
		role: 'assistant',
		content,
		sources,
		timestamp: new Date(),
	};
	messages.update(m => [...m, msg]);
	return msg;
}

export function clearChat() {
	messages.set([]);
	sessionId.set(crypto.randomUUID());
}
