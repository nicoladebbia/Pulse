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
			};
		}
	| { event: 'Error'; data: { message: string } };

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
	financial_stories: FreedomStory[];
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

// === Trend Types ===

export interface TrendPoint {
	date: string;
	headline: string;
	significance: number;
}

export interface TrendThread {
	id: number;
	title: string;
	sector: string;
	trajectory: string;
	acceleration: number;
	points: TrendPoint[];
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

export interface UsageStats {
	period: string;
	total_cost_usd: number;
	total_input_tokens: number;
	total_output_tokens: number;
	by_provider: ProviderUsage[];
}
