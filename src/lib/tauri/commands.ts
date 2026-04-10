import { invoke, Channel } from '@tauri-apps/api/core';
import type { BriefingWithStories, Story, StoryDetail, StoryHeadline, StoryTrendBadge, ChatThread, ChatMessage, ConversationResponse, ChatStreamEvent, ProjectIdea, IdeaStreamEvent, UsageStats, TavilyQuota, TrendThread, ChatContext } from './types';

export async function getTodayBriefing(): Promise<BriefingWithStories | null> {
	return invoke('get_today_briefing');
}

export async function getBriefingByDate(date: string): Promise<BriefingWithStories | null> {
	return invoke('get_briefing_by_date', { date });
}

export async function getStoriesBySector(briefingId: number, sector: string): Promise<Story[]> {
	return invoke('get_stories_by_sector', { briefingId, sector });
}

export async function getStoryDetail(storyId: number): Promise<StoryDetail> {
	return invoke('get_story_detail', { storyId });
}

export async function getStoryHeadlines(storyIds: number[]): Promise<StoryHeadline[]> {
	return invoke('get_story_headlines', { storyIds });
}

export async function fullTextSearch(query: string): Promise<Story[]> {
	return invoke('full_text_search', { query });
}

export async function triggerManualFetch(): Promise<string> {
	return invoke('trigger_manual_fetch');
}

export async function getFetchStatus(): Promise<boolean> {
	return invoke('get_fetch_status');
}

export async function chatSend(threadId: string | null, message: string): Promise<ConversationResponse> {
	return invoke('chat_send', { threadId, message });
}

export async function chatListThreads(): Promise<ChatThread[]> {
	return invoke('chat_list_threads');
}

export async function chatGetThread(threadId: string): Promise<ChatMessage[]> {
	return invoke('chat_get_thread', { threadId });
}

export async function chatDeleteThread(threadId: string): Promise<void> {
	return invoke('chat_delete_thread', { threadId });
}

export async function chatSendStream(
	threadId: string | null,
	message: string,
	onEvent: (event: ChatStreamEvent) => void
): Promise<void> {
	const channel = new Channel<ChatStreamEvent>();
	channel.onmessage = onEvent;
	return invoke('chat_send_stream', { threadId, message, onEvent: channel });
}

// === Ideas ===

export async function generateIdeas(
	onEvent: (event: IdeaStreamEvent) => void
): Promise<void> {
	const channel = new Channel<IdeaStreamEvent>();
	channel.onmessage = onEvent;
	return invoke('generate_ideas', { onEvent: channel });
}

export async function getIdeas(statusFilter?: string): Promise<ProjectIdea[]> {
	return invoke('get_ideas', { statusFilter: statusFilter ?? null });
}

export async function updateIdeaStatus(id: number, status: string): Promise<void> {
	return invoke('update_idea_status', { id, status });
}

// === Trend Badges ===

export async function getStoryTrendBadges(storyIds: number[]): Promise<StoryTrendBadge[]> {
	return invoke('get_story_trend_badges', { storyIds });
}

// === Chat Context ===

export async function getChatContext(): Promise<ChatContext> {
	return invoke('get_chat_context');
}

// === Trends ===

export async function getTrends(): Promise<TrendThread[]> {
	return invoke('get_trends');
}

// === API Usage ===

export async function getApiUsage(days: number): Promise<UsageStats> {
	return invoke('get_api_usage', { days });
}

export async function getTavilyQuota(): Promise<TavilyQuota> {
	return invoke('get_tavily_quota');
}
