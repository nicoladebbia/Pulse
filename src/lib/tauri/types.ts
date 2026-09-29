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
				message_id: string;
				/** Everything retrieved, whether or not the answer used it. */
				source_story_ids: number[];
				/** Only what the answer actually cited, in citation order. */
				cited_story_ids: number[];
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
	/** Terminal state of the most recent run — drives the always-visible last-run line. */
	last_status: 'complete' | 'failed' | 'interrupted' | 'running' | 'idle';
	/** Human-readable reason when last_status === 'failed'. */
	last_reason: string | null;
	/** RFC3339 timestamp of the last progress write (for "Updated 2h ago"). */
	last_at: string | null;
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
	whoop_stories: FreedomStory[];
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

export type PredictionStatus =
	| 'active'
	| 'validated'
	| 'partially_validated'
	| 'invalidated'
	| 'expired'
	| 'needs_review';

export type ResolutionMethod = 'market' | 'llm' | 'manual';

export interface TargetMetric {
	ticker?: string | null;
	operator?: string | null; // e.g. ">=", "<=", "=="
	value?: number | null;
	unit?: string | null;      // "usd", "pct", "count", ...
	baseline_date?: string | null;
	// LLM may produce extra descriptive fields — keep permissive.
	[key: string]: unknown;
}

export interface Prediction {
	id: number | null;
	title: string;
	prediction: string;
	confidence: number;
	reasoning: string;
	evidence_types: string[];
	evidence_story_ids: number[];
	predicted_timeframe: string;
	sector: string | null;
	status: PredictionStatus;
	probability_history: number[];
	// --- v2 fields (nullable on legacy rows) ---
	target_metric?: TargetMetric | null;
	target_date?: string | null;
	source_story_ids?: number[];
	source_signal_ids?: number[];
	model_used?: string | null;
	resolution_method?: ResolutionMethod | null;
	resolution_attempts?: number;
	brier_score?: number | null;
	actual_outcome?: string | null;
	created_at?: string | null;
}

export interface PredictionStats {
	total: number;
	active: number;
	validated: number;
	partially_validated: number;
	invalidated: number;
	expired: number;
	needs_review: number;
	accuracy_rate: number;
	avg_brier_score: number | null;
}

export interface CalibrationBucket {
	accuracy: number;
	n: number;
}

export interface CalibrationStats {
	computed_at: string | null;
	total_resolved: number;
	accuracy_overall: number | null;
	avg_brier: number | null;
	accuracy_by_confidence: Record<string, CalibrationBucket>;
	accuracy_by_topic: Record<string, CalibrationBucket>;
	accuracy_by_timeframe: Record<string, CalibrationBucket>;
	accuracy_by_source: Record<string, CalibrationBucket>;
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

/// Lazy expand-in-place detail for one trend. Deliberately not part of
/// TrendThread: get_trends builds 15 cards at page open and this is 20 stories
/// plus two more queries per card.
export interface TrendDossier {
	topic: string;
	stories: DossierStory[];
	predictions: DossierPrediction[];
	related_entities: RelatedEntity[];
}

export interface DossierStory {
	story_id: number;
	date: string;
	headline: string;
	sector: string;
	what_to_watch: string | null;
}

export interface DossierPrediction {
	title: string;
	confidence: number;
	status: string;
	predicted_timeframe: string;
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
	limit_per_hour: number;    // -1 = no limit (carries hourly cap for FEC etc.)
	limit_per_day: number;     // -1 = no limit
	last_call_at: string | null;
}

// === Weight Calibration Review (pending_calibration — distinct from CalibrationStats above,
// which is prediction accuracy, not trading-weight calibration) ===

export interface PendingCalibrationRow {
	id: number;
	batch_id: string;
	computed_at: string;
	dimension: string;
	old_weight: number;
	new_weight: number;
	hit_rate: number | null;
	sample_size: number | null;
	total_resolved: number;
	status: string;
	/** Set when this row cannot be applied. Optional so existing fixtures and
	 *  mocks stay valid; absent or null means applicable. */
	stale_reason?: string | null;
}

export interface CalibrationGateStatus {
	total_resolved: number;
	threshold: number;
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
	trade_journal: string | null;
}

// === Trade Detail (forensics page) ===

export interface TradeSignalSnapshot {
	computed_at: string | null;
	compound_score: number;
	insider: number;
	institutional: number;
	news: number;
	government: number;
	search: number;
	patent: number;
	supply_chain: number;
	political: number;
	source_diversity: number;
	convergence_detected: boolean;
}

export interface TradeExitPlan {
	current_price: number;
	price_date: string | null;
	hard_stop_price: number;
	fixed_stop_price: number;
	no_atr_fallback: boolean;
	atr: number;
	atr_mult: number;
	high_water_mark: number | null;
	stored_trailing_stop: number | null;
	live_trailing_stop: number | null;
	profit_target_price: number | null;
	half_closed_at: string | null;
	days_held: number;
	max_hold_date: string | null;
	days_remaining: number | null;
	decay_original_score: number;
	decay_current_score: number;
	decay_threshold: number;
	decay_triggered: boolean;
}

export interface TradeSizing {
	score: number;
	tier_pct: number;
	notional: number;
	entry_floor: number;
	entry_cap: number;
	clamped: boolean;
	implied_sizing_base: number | null;
	scale_in_count: number;
}

export interface TradeDetail {
	trade: PaperTrade;
	entity_name: string | null;
	created_at: string | null;
	alpaca_order_id: string | null;
	entry_signals: TradeSignalSnapshot | null;
	current_signals: TradeSignalSnapshot | null;
	exit_plan: TradeExitPlan | null;
	sizing: TradeSizing;
}

export interface TradeRationale {
	text: string;
	generated_at: string;
	cached: boolean;
}

// === Portfolio Analytics ===

export interface PortfolioAnalytics {
	total_trades: number;
	open_count: number;
	win_rate: number;
	loss_rate: number;
	avg_win_pct: number;
	avg_loss_pct: number;
	profit_factor: number;
	total_return_pct: number;
	total_return_dollars: number;
	sharpe_ratio: number;
	sortino_ratio: number;
	max_drawdown_pct: number;
	avg_holding_days: number;
	best_trade: TradeSummary | null;
	worst_trade: TradeSummary | null;
	signal_attribution: SignalAttribution[];
	sector_exposure: SectorExposure[];
	monthly_returns: MonthlyReturn[];
	equity_curve: EquityPoint[];
}

export interface TradeSummary {
	ticker: string;
	pnl_pct: number;
	pnl_dollars: number;
	entry_date: string;
	exit_date: string;
}

export interface SignalAttribution {
	dimension: string;
	avg_on_wins: number;
	avg_on_losses: number;
	predictive_edge: number;
	sample_count: number;
}

export interface SectorExposure {
	sector: string;
	trade_count: number;
	total_pnl: number;
	win_rate: number;
}

export interface MonthlyReturn {
	month: string;
	pnl_dollars: number;
	pnl_pct: number;
	trade_count: number;
}

export interface EquityPoint {
	date: string;
	value: number;
}

// === Live Price Streaming ===

export interface PriceUpdate {
	symbol: string;
	price: number;
	volume: number;
	timestamp: number;
}

export interface StreamStatus {
	connected: boolean;
	symbols: string[];
	last_update: string | null;
}

// === Backtester ===

export interface BacktestConfig {
	start_date: string;
	end_date: string;
	min_score: number;
	stop_loss_pct: number;
	take_profit_pct: number;
	max_hold_days: number;
	max_positions: number;
	position_size_pct: number;
}

export interface BacktestEquityPoint {
	date: string;
	value: number;
}

export interface BacktestMonthlyReturn {
	month: string;          // "YYYY-MM"
	pnl_dollars: number;
	pnl_pct: number;
	trade_count: number;
}

export interface BacktestResult {
	config_summary: string;
	total_signals: number;
	/** Distinct tickers among those signals. 0 on results read back from
	 *  history — the column isn't persisted, so 0 means unknown, not none. */
	tickers_signalled: number;
	/** Of those, how many ever had a price bar on a day they signalled — the only
	 *  ones the walk could admit. A gap here means the result covers part of the
	 *  signal's universe. Not a live-trading limit; live entries price off Alpaca. */
	tickers_tradable: number;
	/** Closed exits only. Positions still open when the price data ran out are
	 *  counted in `open_at_end` — they move the equity curve but are not outcomes,
	 *  so hit_rate, avg_return_pct and avg_holding_days all exclude them. */
	trades_taken: number;
	open_at_end: number;
	trades_won: number;
	trades_lost: number;
	hit_rate: number;
	avg_return_pct: number;
	total_return_pct: number;
	max_drawdown_pct: number;
	sharpe_ratio: number;
	avg_holding_days: number;
	starting_equity: number;
	ending_equity: number;
	trades: BacktestTrade[];
	equity_curve?: BacktestEquityPoint[];
	monthly_returns?: BacktestMonthlyReturn[];
}

export interface BacktestTrade {
	ticker: string;
	entity_name: string;
	entry_date: string;
	entry_price: number;
	exit_date: string;
	exit_price: number;
	pnl_pct: number;
	pnl_dollars: number;
	holding_days: number;
	exit_reason: string;
	compound_score: number;
	signal_profile: string;
}

// === Trade Journal ===

export interface TradeJournal {
	trade_id: number;
	journal: string;
	signal_breakdown: SignalEntry[];
}

export interface SignalEntry {
	dimension: string;
	entry_value: number;
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
	price_date: string | null;      // staleness indicator for price
	computed_at: string | null;     // staleness indicator for signal
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
	open: number | null;
	high: number | null;
	low: number | null;
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
	computed_at: string | null;
}
