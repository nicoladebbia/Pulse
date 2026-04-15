export interface Story {
	id: number;
	briefing_id: number;
	sector: string;
	headline: string;
	summary: string;
	key_facts: string[];
	why_it_matters: string;
	what_to_watch: string;
	importance_score: number;
	relevance_score: number | null;
	relevance_reason: string | null;
	is_hero: boolean;
	display_order: number;
	original_url: string;
	source_name: string;
	published_at: string | null;
	created_at: string;
	summary_depth: string | null;
	deep_summary: string | null;
	source_type: string | null;
	financial_metadata: string | null;
}

export interface StorySource {
	source_name: string;
	article_url: string;
	is_primary: boolean;
}

export interface CrossConnection {
	connected_story_id: number;
	connected_headline: string;
	connected_sector: string;
	connection_text: string;
	insight_text: string;
}

export interface StoryDetail extends Story {
	sources: StorySource[];
	connections: CrossConnection[];
}

export interface Briefing {
	id: number;
	date: string;
	story_count: number;
	ai_count: number;
	miami_count: number;
	italy_count: number;
	tech_count: number;
	status: string;
	created_at: string;
	briefing_type: string;
	executive_summary: string | null;
	time_label: string | null;
	hero_headline: string | null;
}

export interface BriefingConnection {
	story_id_a: number;
	headline_a: string;
	sector_a: string;
	story_id_b: number;
	headline_b: string;
	sector_b: string;
	connection_text: string;
	insight_text: string;
}

export interface BriefingWithStories {
	briefing: Briefing;
	stories: Story[];
	hero_story: Story | null;
	connections: BriefingConnection[];
}

// === New Chat Types ===

export interface ChatThread {
	id: string;
	topic: string;
	title: string | null;
	created_at: string;
	updated_at: string;
}

export interface ChatMessage {
	id: string;
	thread_id: string;
	role: 'user' | 'assistant' | 'system';
	content: string;
	sources: number[] | null;
	metadata: Record<string, unknown> | null;
	created_at: string;
}

export interface ProactiveInsight {
	story_id: number;
	headline: string;
	date: string;
	connection: string;
}

export interface ConversationResponse {
	message: string;
	source_story_ids: number[];
	suggested_followups: string[];
	thread_topic: string;
	thread_title: string | null;
	proactive_connections: ProactiveInsight[];
}

// === Streaming Chat Types ===

export interface SearchStep {
	stage: string;
	detail: string;
	done: boolean;
}

export type ChatStreamEvent =
	| { event: 'Searching'; data: { stage: string; detail: string } }
	| { event: 'Delta'; data: { text: string } }
	| {
			event: 'Complete';
			data: {
				message: string;
				source_story_ids: number[];
				suggested_followups: string[];
				thread_topic: string;
				thread_title: string | null;
				proactive_connections: ProactiveInsight[];
				search_source: string;
				estimated_cost: number;
				model_used: string;
			};
		}
	| { event: 'Error'; data: { message: string } };

// === Trend Badge Types ===

export interface StoryTrendBadge {
	story_id: number;
	entity: string;
	trajectory: string;
	mention_count: number;
}

// === Story Headline Types ===

export interface StoryHeadline {
	id: number;
	headline: string;
	sector: string;
	date: string;
}

// === Chat Context Types ===

export interface ChatSuggestion {
	text: string;
	category: string;
}

export interface ChatContext {
	greeting: string;
	suggestions: ChatSuggestion[];
	story_count: number;
	briefing_days: number;
	entity_count: number;
}

// === Fetch Status Types ===

export interface FetchStatus {
	running: boolean;
	stage: string | null;
	stage_label: string | null;
	percent: number | null;
	detail: string | null;
	elapsed_secs: number | null;
	eta_secs: number | null;
}

// === Freedom Types ===

export interface FreedomStory {
	id: number;
	freedom: string;
	headline: string;
	summary: string;
	key_facts: string[];
	why_it_matters: string;
	what_to_watch: string;
	importance_score: number;
	is_hero: boolean;
	original_url: string;
	source_name: string;
	published_at: string | null;
}

export interface FreedomsBriefing {
	date: string;
	summary: string | null;
	time_stories: FreedomStory[];
	wealth_stories: FreedomStory[];
	location_stories: FreedomStory[];
	health_stories: FreedomStory[];
}

// === Project Ideas Types ===

export interface Competitor {
	name: string;
	url: string;
	weakness: string;
}

export interface ProjectIdea {
	id: number;
	title: string;
	description: string;
	why_now: string;
	competitors: Competitor[];
	differentiation: string;
	tech_stack: string;
	difficulty: 'weekend' | 'week' | 'month';
	relevance_score: number;
	source_story_ids: number[];
	status: 'new' | 'saved' | 'dismissed' | 'building';
	created_at: string;
}

export type IdeaStreamEvent =
	| { event: 'Progress'; data: { stage: string; detail: string } }
	| { event: 'Complete'; data: { ideas: ProjectIdea[] } }
	| { event: 'Error'; data: { message: string } };

// === Prediction Types ===

export interface Prediction {
	id: number;
	title: string;
	prediction: string;
	confidence: number;
	reasoning: string;
	evidence_types: string[];
	evidence_story_ids: number[];
	predicted_timeframe: string;
	sector: string | null;
	status: 'active' | 'validated' | 'partially_validated' | 'invalidated' | 'expired';
	probability_history: number[];
}

export interface PredictionStats {
	total: number;
	active: number;
	validated: number;
	partially_validated: number;
	invalidated: number;
	expired: number;
	accuracy_rate: number;
	avg_brier_score: number | null;
}

// === Story Entity Context ===

export interface StoryEntityContext {
	name: string;
	entity_type: string | null;
	trajectory: string;
	acceleration: number;
	sentiment: number;
}

// === Trend Types ===

export interface TrendPoint {
	story_id: number;
	date: string;
	headline: string;
	significance: number;
}

export interface RelatedEntity {
	name: string;
	strength: number;
}

export interface TrendThread {
	id: number;
	title: string;
	sector: string;
	trajectory: string;
	acceleration: number;
	mention_count: number;
	days_active: number;
	sparkline: number[];
	points: TrendPoint[];
	sentiment_avg: number;
	related_entities: RelatedEntity[];
	sectors: string[];
	causal_consequence: string | null;
	prediction: TrendPrediction | null;
}

export interface TrendPrediction {
	title: string;
	confidence: number;
}

export interface IntelligenceCounts {
	entity_count: number;
	active_prediction_count: number;
	hot_signal_count: number;
}

// === API Usage Types ===

export interface ProviderUsage {
	provider: string;
	model: string;
	total_input_tokens: number;
	total_output_tokens: number;
	total_cost_usd: number;
	call_count: number;
}

export interface TavilyQuota {
	used: number;
	limit: number;
	remaining: number;
	warning: string | null;
}

export interface DailyCost {
	date: string;
	cost: number;
}

export interface UsageStats {
	period: string;
	total_cost_usd: number;
	today_cost_usd: number;
	total_input_tokens: number;
	total_output_tokens: number;
	total_calls: number;
	by_provider: ProviderUsage[];
	daily: DailyCost[];
}

// === Financial API Quota Types ===

export interface FinancialApiQuota {
	provider: string;
	description: string;
	calls_today: number;
	calls_this_hour: number;
	limit_per_minute: number;  // -1 = no limit
	limit_per_day: number;     // -1 = no limit
	last_call_at: string | null;
}

// === Paper Trading Types ===

export interface Portfolio {
	equity: number;
	cash: number;
	buying_power: number;
	portfolio_value: number;
	positions: Position[];
	open_trades: PaperTrade[];
	closed_trades: PaperTrade[];
}

export interface Position {
	symbol: string;
	qty: number;
	avg_entry_price: number;
	current_price: number;
	market_value: number;
	unrealized_pl: number;
	unrealized_pl_pct: number;
	side: string;
}

export interface PaperTrade {
	id: number;
	entity_id: number;
	ticker: string;
	direction: string;
	entry_price: number;
	entry_date: string;
	exit_price: number | null;
	exit_date: string | null;
	position_size: number;
	confidence: number;
	signal_profile: string;
	status: string;
	pnl: number | null;
	pnl_pct: number | null;
}

// === Financial Events ===

export interface FinancialEvent {
	id: number;
	source_name: string;
	feed_id: string;
	headline: string;
	summary: string;
	published_at: string | null;
	sector: string;
	financial_metadata: string | null;
}

// === Signal Evidence (Buy Reasons) ===

export interface SignalEvidence {
	entity_name: string;
	ticker: string | null;
	compound_score: number;
	reasons: string[];
	source_stories: EvidenceStory[];
	price: number | null;
	price_change_1d: number | null;
	recommendation: string;
	position_size_pct: number;
}

export interface EvidenceStory {
	headline: string;
	source_name: string;
	published_at: string | null;
}

// === Source Health ===

export interface SourceHealth {
	name: string;
	status: string;
	last_count: number;
	last_fetch: string | null;
	description: string;
}

// === Market Price Types ===

export interface EntityPrice {
	ticker: string;
	date: string;
	close: number;
	change_1d: number | null;
	change_7d: number | null;
	change_30d: number | null;
	entity_name: string | null;
}

// === Cross-Signal Types ===

export interface CrossSignal {
	entity_id: number;
	entity_name: string;
	ticker: string | null;
	compound_score: number;
	insider_signal: number;
	institutional_flow: number;
	news_momentum: number;
	government_signal: number;
	search_trend: number;
	patent_signal: number;
	supply_chain: number;
	political_signal: number;
	source_diversity: number;
	convergence_detected: boolean;
}
