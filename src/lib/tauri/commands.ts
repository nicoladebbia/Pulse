import { invoke, Channel } from '@tauri-apps/api/core';
import type { BriefingWithStories, Story, StoryDetail, StoryHeadline, StoryTrendBadge, ChatThread, ChatMessage, ConversationResponse, ChatStreamEvent, ProjectIdea, IdeaStreamEvent, Prediction, PredictionStats, IntelligenceCounts, UsageStats, TavilyQuota, TrendThread, ChatContext, CrossSignal, FinancialApiQuota, EntityPrice, Portfolio, PaperTrade, FinancialEvent, SignalEvidence, SourceHealth, FetchStatus, PortfolioAnalytics, TradeJournal, BacktestConfig, BacktestResult, StreamStatus } from './types';

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

export async function getFetchStatus(): Promise<FetchStatus> {
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

// === Predictions ===

export async function getAllPredictions(): Promise<Prediction[]> {
	return invoke('get_all_predictions');
}

export async function getPredictionStats(): Promise<PredictionStats> {
	return invoke('get_prediction_stats');
}

export async function manuallyValidatePrediction(id: number, outcome: string): Promise<void> {
	return invoke('manually_validate_prediction', { id, outcome });
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

export async function getIntelligenceCounts(): Promise<IntelligenceCounts> {
	return invoke('get_intelligence_counts');
}

// === API Usage ===

export async function getApiUsage(days: number): Promise<UsageStats> {
	return invoke('get_api_usage', { days });
}

export async function getTavilyQuota(): Promise<TavilyQuota> {
	return invoke('get_tavily_quota');
}

export async function getFinancialQuotas(): Promise<FinancialApiQuota[]> {
	return invoke('get_financial_quotas');
}

// === Cross-Signal Intelligence ===

export async function getCrossSignals(limit?: number): Promise<CrossSignal[]> {
	return invoke('get_cross_signals', { limit: limit ?? 20 });
}

export async function getConvergenceAlerts(limit?: number): Promise<CrossSignal[]> {
	return invoke('get_convergence_alerts', { limit: limit ?? 10 });
}

export async function getEntityPrices(limit?: number): Promise<EntityPrice[]> {
	return invoke('get_entity_prices', { limit: limit ?? 50 });
}

export async function refreshPrices(): Promise<number> {
	return invoke('refresh_prices');
}

export async function getFinancialEvents(limit?: number): Promise<FinancialEvent[]> {
	return invoke('get_financial_events', { limit: limit ?? 30 });
}

export async function getSignalEvidence(limit?: number): Promise<SignalEvidence[]> {
	return invoke('get_signal_evidence', { limit: limit ?? 10 });
}

export async function getSourceHealth(): Promise<SourceHealth[]> {
	return invoke('get_source_health');
}

// === Paper Trading ===

export async function getPortfolio(): Promise<Portfolio> {
	return invoke('get_portfolio');
}

export async function getPaperTrades(status?: string): Promise<PaperTrade[]> {
	return invoke('get_paper_trades', { status: status ?? 'open' });
}

export async function executeTrade(ticker: string, confidence: number): Promise<PaperTrade | null> {
	return invoke('execute_trade', { ticker, confidence });
}

export async function getPortfolioAnalytics(): Promise<PortfolioAnalytics> {
	return invoke('get_portfolio_analytics');
}

export async function getTradeJournal(tradeId: number): Promise<TradeJournal> {
	return invoke('get_trade_journal', { tradeId });
}

export async function runBacktest(config: BacktestConfig): Promise<BacktestResult> {
	return invoke('run_backtest', { config });
}

export async function getBacktestHistory(): Promise<BacktestResult[]> {
	return invoke('get_backtest_history');
}

export async function startPriceStream(): Promise<void> {
	return invoke('start_price_stream');
}

export async function stopPriceStream(): Promise<void> {
	return invoke('stop_price_stream');
}

export async function getStreamStatus(): Promise<StreamStatus> {
	return invoke('get_stream_status');
}
