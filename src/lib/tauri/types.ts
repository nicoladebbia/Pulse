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
}

export interface BriefingWithStories {
	briefing: Briefing;
	stories: Story[];
	hero_story: Story | null;
}
