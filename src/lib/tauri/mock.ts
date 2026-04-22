import type { BriefingWithStories, Story, Briefing, BriefingConnection, FreedomsBriefing, FreedomStory, ProjectIdea, Prediction, PredictionStats, CalibrationStats, UsageStats, IdeaStreamEvent, TavilyQuota, TrendThread } from './types';

const today = new Date().toISOString().slice(0, 10);

const mockStories: Story[] = [
	{
		id: 1, briefing_id: 1, sector: 'ai',
		headline: 'OpenAI Launches GPT-5 with Real-Time Reasoning',
		summary: 'OpenAI unveiled GPT-5, featuring a new architecture that enables real-time multi-step reasoning. The model demonstrates significant improvements in mathematical proof generation and code synthesis.',
		key_facts: ['GPT-5 uses a hybrid reasoning architecture', 'Benchmarks show 40% improvement over GPT-4 on MATH dataset', 'Available to Plus subscribers immediately'],
		why_it_matters: 'This sets a new bar for foundation models and will pressure competitors to accelerate their own reasoning capabilities.',
		what_to_watch: 'How quickly enterprises adopt GPT-5 for production workloads, and whether the reasoning gains hold up on real-world tasks.',
		importance_score: 9.2, relevance_score: 9.5, relevance_reason: 'Direct impact on AI development landscape',
		is_hero: true, display_order: 1,
		original_url: 'https://example.com/gpt5', source_name: 'TechCrunch',
		published_at: today, created_at: today,
		summary_depth: 'deep', source_type: 'news', financial_metadata: null,
		deep_summary: '**Background**: OpenAI has been racing to maintain its lead in the foundation model space since ChatGPT\'s breakout in late 2022. GPT-4, released in March 2023, set the benchmark for multimodal AI but faced increasing competition from Anthropic\'s Claude, Google\'s Gemini, and open-source models like Llama.\n\n**Key Players**: Sam Altman positioned GPT-5 as a "paradigm shift" in reasoning. Microsoft, as OpenAI\'s primary investor, gains immediate enterprise distribution advantages. Anthropic and Google are likely weeks away from counter-announcements.\n\n**Multiple Angles**: Enterprise users see the reasoning capability as a potential replacement for expensive human review processes. AI safety researchers have expressed concern about the model\'s ability to chain multi-step reasoning without oversight. Open-source advocates argue this widens the capability gap between proprietary and open models.\n\n**Implications**: For AI app builders, GPT-5\'s reasoning API opens new product categories — automated due diligence, real-time strategy analysis, and code architecture planning. The 40% MATH improvement suggests it could handle financial modeling tasks previously requiring specialized systems.\n\n**What Happens Next**: Expect Anthropic to accelerate Claude 4 release timeline. Enterprise adoption will hinge on pricing — if OpenAI maintains GPT-4 pricing levels, expect rapid migration within 90 days.',
	},
	{
		id: 2, briefing_id: 1, sector: 'tech',
		headline: 'Apple Acquires Robotics Startup for $2B',
		summary: 'Apple has acquired a stealth robotics startup specializing in home automation hardware, signaling a push into consumer robotics beyond the Vision Pro ecosystem.',
		key_facts: ['Acquisition valued at $2 billion', 'Startup had 200 employees and 14 patents', 'Products expected to integrate with HomeKit'],
		why_it_matters: 'Apple entering robotics could reshape the smart home market and create a new hardware category.',
		what_to_watch: 'Whether Apple announces a consumer robot product at WWDC this year.',
		importance_score: 8.5, relevance_score: 8.0, relevance_reason: 'Major tech industry move',
		is_hero: false, display_order: 2,
		original_url: 'https://example.com/apple-robotics', source_name: 'Bloomberg',
		published_at: today, created_at: today,
		summary_depth: null, deep_summary: null, source_type: 'news', financial_metadata: null,
	},
	{
		id: 3, briefing_id: 1, sector: 'miami',
		headline: 'Miami Approves $500M Tech District Expansion',
		summary: 'The Miami city commission approved a $500M expansion of the Wynwood tech district, including new co-working spaces, a startup incubator, and transit improvements.',
		key_facts: ['$500M public-private investment', 'Expected to create 5,000 tech jobs by 2028', 'Includes affordable housing component'],
		why_it_matters: 'Solidifies Miami as a top-tier tech hub and could attract more venture capital to the region.',
		what_to_watch: 'Whether major tech companies announce Miami office expansions following this investment.',
		importance_score: 7.8, relevance_score: 8.5, relevance_reason: 'Local Miami tech ecosystem growth',
		is_hero: false, display_order: 3,
		original_url: 'https://example.com/miami-tech', source_name: 'Miami Herald',
		published_at: today, created_at: today,
		summary_depth: null, deep_summary: null, source_type: 'news', financial_metadata: null,
	},
	{
		id: 4, briefing_id: 1, sector: 'italy',
		headline: 'Italy Passes EU-Leading AI Regulation Framework',
		summary: 'The Italian parliament passed comprehensive AI regulation that goes beyond the EU AI Act, establishing a national AI safety board and mandatory impact assessments for high-risk systems.',
		key_facts: ['First EU nation to pass supplementary AI legislation', 'Creates a National AI Safety Board', 'Mandatory audits for public-sector AI'],
		why_it_matters: 'Italy is positioning itself as a leader in responsible AI governance within the EU.',
		what_to_watch: 'How Italian tech companies adapt and whether other EU nations follow suit.',
		importance_score: 7.5, relevance_score: 7.8, relevance_reason: 'Italian tech policy',
		is_hero: false, display_order: 4,
		original_url: 'https://example.com/italy-ai', source_name: 'Corriere della Sera',
		published_at: today, created_at: today,
		summary_depth: null, deep_summary: null, source_type: 'news', financial_metadata: null,
	},
	{
		id: 5, briefing_id: 1, sector: 'ai',
		headline: 'Anthropic Raises $5B Series E at $60B Valuation',
		summary: 'Anthropic closed a $5 billion Series E round, making it one of the largest private AI companies. The funding will accelerate Claude model development and enterprise infrastructure.',
		key_facts: ['$5B raised at $60B valuation', 'Led by a consortium of investors', 'Funds earmarked for compute and safety research'],
		why_it_matters: 'Signals continued investor confidence in frontier AI labs and the race for AGI capabilities.',
		what_to_watch: 'How Anthropic deploys this capital — more compute, more researchers, or new product lines.',
		importance_score: 8.8, relevance_score: 9.0, relevance_reason: 'AI industry funding landscape',
		is_hero: false, display_order: 5,
		original_url: 'https://example.com/anthropic', source_name: 'The Information',
		published_at: today, created_at: today,
		summary_depth: null, deep_summary: null, source_type: 'news', financial_metadata: null,
	},
	{
		id: 6, briefing_id: 1, sector: 'tech',
		headline: 'Rust Surpasses C++ in Developer Satisfaction Survey',
		summary: 'The annual Stack Overflow survey shows Rust as the most loved language for the eighth consecutive year, now also surpassing C++ in adoption for systems programming.',
		key_facts: ['Rust adoption up 35% year-over-year', '87% of Rust developers want to continue using it', 'Major companies migrating core infrastructure to Rust'],
		why_it_matters: 'The shift toward memory-safe languages is accelerating, with implications for security and performance across the industry.',
		what_to_watch: 'Whether the US government mandate for memory-safe languages drives further enterprise Rust adoption.',
		importance_score: 6.5, relevance_score: 7.0, relevance_reason: 'Relevant to Rust-based projects',
		is_hero: false, display_order: 6,
		original_url: 'https://example.com/rust', source_name: 'Stack Overflow Blog',
		published_at: today, created_at: today,
		summary_depth: null, deep_summary: null, source_type: 'news', financial_metadata: null,
	},
	{
		id: 7, briefing_id: 1, sector: 'miami',
		headline: 'South Florida Startup Raises $80M for Climate Tech',
		summary: 'A Coral Gables-based climate tech startup raised $80M to scale its ocean carbon capture technology, with plans to deploy off the coast of Miami-Dade.',
		key_facts: ['$80M Series B', 'Technology captures CO2 from seawater', 'First deployment planned for Biscayne Bay area'],
		why_it_matters: 'Climate tech investment in South Florida is growing, leveraging the region\'s unique vulnerability to climate change.',
		what_to_watch: 'Regulatory approval process and first deployment results.',
		importance_score: 6.8, relevance_score: 7.2, relevance_reason: 'South Florida climate tech',
		is_hero: false, display_order: 7,
		original_url: 'https://example.com/climate', source_name: 'South Florida Business Journal',
		published_at: today, created_at: today,
		summary_depth: null, deep_summary: null, source_type: 'news', financial_metadata: null,
	},
];

const mockBriefing: Briefing = {
	id: 1,
	date: today,
	story_count: mockStories.length,
	ai_count: mockStories.filter(s => s.sector === 'ai').length,
	miami_count: mockStories.filter(s => s.sector === 'miami').length,
	italy_count: mockStories.filter(s => s.sector === 'italy').length,
	tech_count: mockStories.filter(s => s.sector === 'tech').length,
	status: 'complete',
	created_at: today,
	briefing_type: 'daily',
	executive_summary: 'AI dominates today\'s briefing with OpenAI\'s GPT-5 launch and Anthropic\'s massive fundraise signaling an acceleration in the frontier model race. Meanwhile, Miami continues to cement its tech hub status with a $500M district expansion, and Italy takes a leadership role in EU AI governance.',
	time_label: null,
	hero_headline: 'OpenAI Launches GPT-5 with Real-Time Reasoning',
};

const mockConnections: BriefingConnection[] = [
	{
		story_id_a: 1, headline_a: 'OpenAI Launches GPT-5 with Real-Time Reasoning', sector_a: 'ai',
		story_id_b: 5, headline_b: 'Anthropic Raises $5B Series E at $60B Valuation', sector_b: 'ai',
		connection_text: 'The AI arms race intensifies',
		insight_text: 'Both stories reflect the breakneck pace of frontier AI development. GPT-5\'s launch will pressure Anthropic to accelerate Claude releases, while Anthropic\'s fundraise ensures they have the capital to compete.',
	},
	{
		story_id_a: 4, headline_a: 'Italy Passes EU-Leading AI Regulation Framework', sector_a: 'italy',
		story_id_b: 1, headline_b: 'OpenAI Launches GPT-5 with Real-Time Reasoning', sector_b: 'ai',
		connection_text: 'Regulation meets capability',
		insight_text: 'Italy\'s new AI safety board will immediately face the challenge of evaluating GPT-5-class models, testing whether regulation can keep pace with rapid capability gains.',
	},
];

export const mockBriefingData: BriefingWithStories = {
	briefing: mockBriefing,
	stories: mockStories,
	hero_story: mockStories[0],
	connections: mockConnections,
};

// --- Archive mock data ---

function daysAgo(n: number): string {
	const d = new Date();
	d.setDate(d.getDate() - n);
	return d.toISOString().slice(0, 10);
}

export const mockArchiveBriefings: Briefing[] = [
	mockBriefing,
	{ id: 2, date: daysAgo(1), story_count: 5, ai_count: 2, miami_count: 1, italy_count: 1, tech_count: 1, status: 'complete', created_at: daysAgo(1), briefing_type: 'daily', executive_summary: 'Yesterday\'s key developments.' },
	{ id: 3, date: daysAgo(1), story_count: 4, ai_count: 0, miami_count: 0, italy_count: 0, tech_count: 0, status: 'complete', created_at: daysAgo(1), briefing_type: 'freedoms', executive_summary: null },
	{ id: 4, date: daysAgo(2), story_count: 6, ai_count: 3, miami_count: 1, italy_count: 1, tech_count: 1, status: 'complete', created_at: daysAgo(2), briefing_type: 'daily', executive_summary: 'Two days ago highlights.' },
	{ id: 5, date: daysAgo(3), story_count: 4, ai_count: 1, miami_count: 1, italy_count: 1, tech_count: 1, status: 'complete', created_at: daysAgo(3), briefing_type: 'daily', executive_summary: null },
];

// --- Trends mock data ---

export const mockTrends: TrendThread[] = [
	{
		id: 1, title: 'Anthropic', sector: 'ai', trajectory: 'dominant', acceleration: 2.5,
		mention_count: 29, days_active: 14, sparkline: [2, 3, 1, 2, 3, 2, 1, 3, 2, 2, 1, 3, 2, 2],
		points: [
			{ story_id: 1, date: daysAgo(0), headline: 'Claude gains computer control capability for task automation', significance: 8 },
			{ story_id: 2, date: daysAgo(1), headline: 'Anthropic leaks Claude Mythos with dramatically higher benchmarks', significance: 9 },
			{ story_id: 3, date: daysAgo(3), headline: 'Anthropic raises $5B Series E at $60B valuation', significance: 7 },
		],
		sentiment_avg: 0.65, related_entities: [{ name: 'OpenAI', strength: 8 }, { name: 'Google DeepMind', strength: 5 }, { name: 'Claude', strength: 4 }],
		sectors: ['ai', 'tech'], causal_consequence: 'GPU demand',
		prediction: { title: 'Claude will become the default coding assistant', confidence: 0.60 },
	},
	{
		id: 2, title: 'OpenAI', sector: 'ai', trajectory: 'hot', acceleration: 2.4,
		mention_count: 25, days_active: 14, sparkline: [1, 2, 3, 2, 1, 2, 2, 1, 3, 2, 1, 2, 1, 2],
		points: [
			{ story_id: 4, date: daysAgo(0), headline: 'OpenAI preps Operator agentic AI launch', significance: 8 },
			{ story_id: 5, date: daysAgo(2), headline: 'GPT-5 timeline confirmed for Q2 release', significance: 9 },
		],
		sentiment_avg: 0.35, related_entities: [{ name: 'Anthropic', strength: 8 }, { name: 'Microsoft', strength: 7 }],
		sectors: ['ai', 'tech', 'miami'], causal_consequence: 'AI regulation',
		prediction: null,
	},
	{
		id: 3, title: 'Giorgia Meloni', sector: 'italy', trajectory: 'rising', acceleration: 2.6,
		mention_count: 11, days_active: 9, sparkline: [0, 1, 0, 1, 2, 0, 1, 1, 0, 1, 1, 1, 1, 0],
		points: [
			{ story_id: 6, date: daysAgo(1), headline: 'Meloni pushes new AI governance framework in EU', significance: 6 },
			{ story_id: 7, date: daysAgo(4), headline: 'Italy-US tech partnership announced at bilateral summit', significance: 5 },
		],
		sentiment_avg: -0.15, related_entities: [{ name: 'EU', strength: 4 }],
		sectors: ['italy'], causal_consequence: null,
		prediction: null,
	},
	{
		id: 4, title: 'SpaceX', sector: 'tech', trajectory: 'rising', acceleration: 3.8,
		mention_count: 7, days_active: 7, sparkline: [0, 0, 1, 0, 1, 0, 0, 1, 1, 0, 1, 1, 0, 1],
		points: [
			{ story_id: 8, date: daysAgo(0), headline: 'SpaceX Starship completes first orbital refueling test', significance: 9 },
			{ story_id: 9, date: daysAgo(3), headline: 'SpaceX wins $3B NASA Artemis lunar cargo contract', significance: 7 },
		],
		sentiment_avg: 0.8, related_entities: [{ name: 'NASA', strength: 6 }, { name: 'Blue Origin', strength: 3 }],
		sectors: ['tech'], causal_consequence: 'Satellite internet',
		prediction: null,
	},
];

// --- Freedoms mock data ---

const makeFreedomStory = (id: number, freedom: string, headline: string, summary: string): FreedomStory => ({
	id, freedom, headline, summary,
	key_facts: ['Key insight about this topic', 'Supporting data point', 'Expert perspective'],
	why_it_matters: 'This development directly impacts your path to greater freedom.',
	what_to_watch: 'Monitor upcoming developments in this space over the next quarter.',
	importance_score: 7 + Math.random() * 2,
	is_hero: id % 4 === 0,
	original_url: 'https://example.com',
	source_name: 'Mock Source',
	published_at: today,
});

export const mockFreedomsBriefing: FreedomsBriefing = {
	date: today,
	summary: 'AI agents colonize the productivity stack while Europe standardizes the 4-day week, reshaping time and location freedom simultaneously.',
	time_stories: [
		makeFreedomStory(100, 'time', 'AI Agents Now Handle 60% of Routine Dev Tasks', 'New study shows AI coding assistants are dramatically reducing time spent on boilerplate and debugging.'),
		makeFreedomStory(101, 'time', 'The 4-Day Work Week Goes Mainstream in Europe', 'Following successful trials, three more EU countries announce nationwide 4-day week pilots.'),
	],
	wealth_stories: [
		makeFreedomStory(104, 'wealth', 'Micro-SaaS Founders Report Record Bootstrapped Revenue', 'The indie SaaS community reports a 40% increase in solo-founder revenue, driven by AI-assisted development.'),
		makeFreedomStory(105, 'wealth', 'New Tax-Advantaged Accounts for Digital Nomads', 'US introduces a new savings vehicle designed for location-independent workers.'),
	],
	location_stories: [
		makeFreedomStory(108, 'location', 'Portugal Launches Enhanced Digital Nomad Visa 2.0', 'Updated visa program offers faster processing, lower income thresholds, and a path to residency.'),
	],
	health_stories: [
		makeFreedomStory(112, 'health', 'Rapamycin Analog Shows Promise in Human Longevity Trial', 'First large-scale human trial of a rapamycin derivative shows 15% reduction in aging biomarkers.'),
		makeFreedomStory(113, 'health', 'Continuous Glucose Monitors for Non-Diabetics Go Mainstream', 'CGM adoption among healthy adults triples as metabolic health awareness grows.'),
	],
};

// --- Chat mock streaming ---

const mockChatResponse = `Here's what I found in your news archive:

## Key Developments

**AI & LLMs** are dominating today's cycle. OpenAI's GPT-5 launch represents a significant leap in reasoning capabilities, while Anthropic's $5B raise signals the AI arms race is far from over.

### Cross-Sector Connections

- The **Miami tech district expansion** could attract more AI startups to South Florida
- Italy's **AI regulation framework** will directly impact how GPT-5-class models are deployed in the EU

### What to Watch

1. Enterprise adoption rates for GPT-5
2. Whether Anthropic accelerates Claude releases in response
3. How Italy's safety board evaluates frontier models

[followup: "How does Italy's regulation compare to the EU AI Act?"]
[followup: "What's driving Miami's tech growth?"]`;

export function simulateChatStream(onEvent: (event: any) => void): Promise<void> {
	return new Promise((resolve) => {
		const words = mockChatResponse.split(' ');
		let i = 0;
		const interval = setInterval(() => {
			if (i < words.length) {
				const chunk = words[i] + (i < words.length - 1 ? ' ' : '');
				onEvent({ event: 'Delta', data: { text: chunk } });
				i++;
			} else {
				clearInterval(interval);
				onEvent({
					event: 'Complete',
					data: {
						message: mockChatResponse.replace(/\[followup:.*?\]/g, '').replace(/\[story:\d+\]/g, '').trim(),
						source_story_ids: [1, 4, 5],
						suggested_followups: [
							"How does Italy's regulation compare to the EU AI Act?",
							"What's driving Miami's tech growth?",
						],
						thread_topic: 'general',
						thread_title: 'News Archive Overview',
						proactive_connections: [],
					},
				});
				resolve();
			}
		}, 30);
	});
}

// --- Fetch progress mock ---

const mockStages = [
	{ stage: 'collecting', stage_label: 'Collecting sources', percent: 3, detail: null },
	{ stage: 'deduplicating', stage_label: 'Deduplicating articles', percent: 7, detail: '847 → 156 articles' },
	{ stage: 'summarizing', stage_label: 'Summarizing stories', percent: 15, detail: 'Batch 2/15 (20 done)' },
	{ stage: 'summarizing', stage_label: 'Summarizing stories', percent: 28, detail: 'Batch 6/15 (60 done)' },
	{ stage: 'summarizing', stage_label: 'Summarizing stories', percent: 42, detail: 'Batch 10/15 (100 done)' },
	{ stage: 'analyzing', stage_label: 'Cross-sector analysis', percent: 55, detail: null },
	{ stage: 'executive_summary', stage_label: 'Executive summary', percent: 65, detail: null },
	{ stage: 'contextual', stage_label: 'Contextual prefixes', percent: 72, detail: null },
	{ stage: 'embeddings', stage_label: 'Generating embeddings', percent: 78, detail: null },
	{ stage: 'writing_db', stage_label: 'Writing to database', percent: 82, detail: null },
	{ stage: 'entities', stage_label: 'Extracting entities', percent: 90, detail: 'Batch 3/6' },
	{ stage: 'complete', stage_label: 'Complete', percent: 100, detail: null },
];

let mockFetchStageIdx = 0;
let mockFetchStartTime = 0;

export function startMockFetch() {
	mockFetchStageIdx = 0;
	mockFetchStartTime = Date.now();
}

export function getMockFetchStatus(): { running: boolean; stage: string | null; stage_label: string | null; percent: number | null; detail: string | null; elapsed_secs: number | null; eta_secs: number | null } {
	if (mockFetchStageIdx >= mockStages.length) {
		return { running: false, stage: null, stage_label: null, percent: null, detail: null, elapsed_secs: null, eta_secs: null };
	}

	const s = mockStages[mockFetchStageIdx];
	const elapsed = Math.floor((Date.now() - mockFetchStartTime) / 1000);
	const eta = s.percent > 0 ? Math.max(0, Math.floor(elapsed / (s.percent / 100) - elapsed)) : null;
	mockFetchStageIdx++;

	if (s.stage === 'complete') {
		return { running: false, stage: s.stage, stage_label: s.stage_label, percent: 100, detail: null, elapsed_secs: elapsed, eta_secs: 0 };
	}

	return { running: true, ...s, elapsed_secs: elapsed, eta_secs: eta };
}

export function isTauri(): boolean {
	return !!(window as any).__TAURI_INTERNALS__;
}

// === Mock Ideas ===

export const mockIdeas: ProjectIdea[] = [
	{
		id: 1, title: 'AI Code Review Bot for Shopify Apps',
		description: 'An automated code review tool that uses Claude to analyze Shopify app submissions for common pitfalls, security issues, and performance problems.',
		why_now: 'With Shopify\'s app ecosystem growing 30% YoY and AI code review becoming mainstream, there\'s a gap for Shopify-specific tooling.',
		competitors: [
			{ name: 'CodeRabbit', url: 'https://coderabbit.ai', weakness: 'Generic — no Shopify-specific checks' },
			{ name: 'Sourcery', url: 'https://sourcery.ai', weakness: 'Python-focused, doesn\'t support Liquid templates' },
		],
		differentiation: 'Deep Shopify API knowledge, Liquid template analysis, and integration with Shopify Partner Dashboard.',
		tech_stack: 'TypeScript, Claude API, GitHub Actions',
		difficulty: 'week', relevance_score: 0.9,
		source_story_ids: [1, 2],
		status: 'new', created_at: today,
	},
	{
		id: 2, title: 'Miami Real Estate AI Agent',
		description: 'A conversational AI that helps Miami homebuyers navigate the market using real-time MLS data, neighborhood analysis, and price prediction.',
		why_now: 'Miami real estate is booming with tech migration, but existing tools are outdated. AI agents are the new interface for complex data.',
		competitors: [
			{ name: 'Zillow', url: 'https://zillow.com', weakness: 'Generic national tool, weak on Miami micro-markets' },
		],
		differentiation: 'Hyper-local Miami knowledge, conversational interface, integrates neighborhood data (crime, schools, walkability).',
		tech_stack: 'Python, Claude API, React, MLS API',
		difficulty: 'month', relevance_score: 0.75,
		source_story_ids: [3, 4],
		status: 'new', created_at: today,
	},
	{
		id: 3, title: 'Serie A Stats Dashboard',
		description: 'A real-time Serie A analytics dashboard combining match data, player performance, and betting market signals.',
		why_now: 'Serie A is gaining global viewership and sports analytics tools are still dominated by EPL-focused products.',
		competitors: [],
		differentiation: 'Italian-language support, integration with local betting markets, and AI-generated match previews.',
		tech_stack: 'SvelteKit, Python, DuckDB',
		difficulty: 'weekend', relevance_score: 0.65,
		source_story_ids: [5],
		status: 'saved', created_at: daysAgo(2),
	},
];

export function simulateIdeaGeneration(onEvent: (event: IdeaStreamEvent) => void) {
	const stages = [
		{ stage: 'loading', detail: 'Loading recent stories...', delay: 500 },
		{ stage: 'generating', detail: 'Analyzing trends and generating project ideas...', delay: 2000 },
		{ stage: 'researching', detail: 'Researching competitors...', delay: 1500 },
		{ stage: 'saving', detail: 'Saving ideas...', delay: 300 },
	];

	let i = 0;
	const next = () => {
		if (i < stages.length) {
			onEvent({ event: 'Progress', data: stages[i] });
			const delay = stages[i].delay;
			i++;
			setTimeout(next, delay);
		} else {
			onEvent({ event: 'Complete', data: { ideas: mockIdeas.filter(id => id.status === 'new') } });
		}
	};
	next();
}

// === Mock Predictions ===

export const mockPredictions: Prediction[] = [
	{ id: 1, title: 'GPT-5 will surpass human performance on ARC-AGI', prediction: 'Within 6 months of release, GPT-5 will achieve >90% on ARC-AGI benchmark', confidence: 0.72, reasoning: 'Based on trajectory of improvements and OpenAI\'s focus on reasoning', evidence_types: ['signal', 'causal_chain'], evidence_story_ids: [1, 3], predicted_timeframe: '2026-09-01', sector: 'ai', status: 'active', probability_history: [0.55, 0.60, 0.65, 0.68, 0.72] },
	{ id: 2, title: 'Miami tech hub will attract 3 major AI companies', prediction: 'At least 3 AI companies valued >$1B will establish Miami offices', confidence: 0.65, reasoning: 'Tax incentives + growing talent pool + recent announcements', evidence_types: ['signal'], evidence_story_ids: [5], predicted_timeframe: '2026-12-31', sector: 'miami', status: 'active', probability_history: [0.50, 0.55, 0.60, 0.65] },
	{ id: 3, title: 'EU AI Act enforcement will cause compliance rush', prediction: 'Major SaaS companies will scramble to comply with EU AI Act by Q3 2026', confidence: 0.85, reasoning: 'Regulatory deadline approaching with most companies unprepared', evidence_types: ['pattern', 'signal'], evidence_story_ids: [2, 7], predicted_timeframe: '2026-08-01', sector: 'tech', status: 'active', probability_history: [0.70, 0.75, 0.80, 0.82, 0.85] },
	{ id: 4, title: 'Italy Serie A streaming deal will double in value', prediction: 'Next Serie A broadcast rights deal will exceed €2B', confidence: 0.55, reasoning: 'Growing international interest and streaming competition', evidence_types: ['signal'], evidence_story_ids: [8], predicted_timeframe: '2027-01-01', sector: 'italy', status: 'active', probability_history: [0.60, 0.58, 0.55] },
	{ id: 5, title: 'Claude will become the default coding assistant', prediction: 'Anthropic\'s Claude will surpass GitHub Copilot in developer adoption', confidence: 0.60, reasoning: 'Claude Code adoption rate and developer satisfaction metrics', evidence_types: ['causal_chain', 'pattern'], evidence_story_ids: [1, 4], predicted_timeframe: '2026-06-15', sector: 'ai', status: 'partially_validated', probability_history: [0.40, 0.45, 0.50, 0.55, 0.58, 0.60] },
	{ id: 6, title: 'Apple Vision Pro price drop', prediction: 'Apple will release a sub-$2000 Vision Pro variant', confidence: 0.78, reasoning: 'Supply chain leaks and competitive pressure from Meta Quest', evidence_types: ['signal'], evidence_story_ids: [6], predicted_timeframe: '2026-03-01', sector: 'tech', status: 'validated', probability_history: [0.65, 0.70, 0.75, 0.78] },
	{ id: 7, title: 'Crypto winter ends with ETF approvals', prediction: 'Bitcoin will reach new ATH following spot ETF approval wave', confidence: 0.70, reasoning: 'Institutional demand + regulatory clarity', evidence_types: ['pattern', 'signal'], evidence_story_ids: [9], predicted_timeframe: '2025-12-01', sector: 'tech', status: 'invalidated', probability_history: [0.80, 0.75, 0.70, 0.60, 0.50] },
	{ id: 8, title: 'Remote work mandate reversal in Big Tech', prediction: 'At least 2 FAANG companies will reverse RTO mandates', confidence: 0.40, reasoning: 'Employee pushback and productivity data', evidence_types: ['contrarian'], evidence_story_ids: [10], predicted_timeframe: '2026-01-01', sector: 'tech', status: 'expired', probability_history: [0.55, 0.50, 0.45, 0.40] },
	{ id: 9, title: 'Italian fintech regulation modernization', prediction: 'Italy will pass comprehensive fintech sandbox legislation', confidence: 0.50, reasoning: 'EU pressure and competition from other member states', evidence_types: ['pattern'], evidence_story_ids: [11], predicted_timeframe: '2026-07-01', sector: 'italy', status: 'active', probability_history: [0.50] },
];

export const mockPredictionStats: PredictionStats = {
	total: 10,
	active: 5,
	validated: 1,
	partially_validated: 1,
	invalidated: 1,
	expired: 1,
	needs_review: 1,
	accuracy_rate: 0.667,
	avg_brier_score: null,
};

export const mockCalibrationStats: CalibrationStats = {
	computed_at: '2026-04-21T12:00:00Z',
	total_resolved: 62,
	accuracy_overall: 0.58,
	avg_brier: 0.21,
	accuracy_by_confidence: {
		'50-60': { accuracy: 0.42, n: 12 },
		'60-70': { accuracy: 0.55, n: 18 },
		'70-80': { accuracy: 0.71, n: 14 },
		'80-90': { accuracy: 0.78, n: 9 },
	},
	accuracy_by_topic: {
		ai: { accuracy: 0.65, n: 22 },
		tech: { accuracy: 0.55, n: 15 },
		miami: { accuracy: 0.48, n: 8 },
	},
	accuracy_by_timeframe: {
		'7d': { accuracy: 0.72, n: 11 },
		'30d': { accuracy: 0.58, n: 27 },
		'90d': { accuracy: 0.44, n: 14 },
	},
	accuracy_by_source: {
		'Hacker News': { accuracy: 0.68, n: 19 },
		'Google News': { accuracy: 0.52, n: 23 },
	},
};

// === Mock Usage Stats ===

export const mockUsageStats: UsageStats = {
	period: 'last 7 days',
	total_cost_usd: 0.47,
	today_cost_usd: 0.08,
	total_input_tokens: 285000,
	total_output_tokens: 42000,
	total_calls: 188,
	by_provider: [
		{ provider: 'anthropic', model: 'claude-sonnet', total_input_tokens: 180000, total_output_tokens: 35000, total_cost_usd: 0.38, call_count: 23 },
		{ provider: 'groq', model: 'llama-3.3-70b', total_input_tokens: 85000, total_output_tokens: 5000, total_cost_usd: 0.05, call_count: 80 },
		{ provider: 'voyage', model: 'voyage-3-lite', total_input_tokens: 20000, total_output_tokens: 0, total_cost_usd: 0.0004, call_count: 80 },
		{ provider: 'tavily', model: 'search', total_input_tokens: 0, total_output_tokens: 0, total_cost_usd: 0, call_count: 5 },
	],
	daily: [
		{ date: '2026-04-17', cost_usd: 0.08, call_count: 28 },
		{ date: '2026-04-16', cost_usd: 0.12, call_count: 45 },
		{ date: '2026-04-15', cost_usd: 0.09, call_count: 32 },
	],
};

export const mockTavilyQuota: TavilyQuota = {
	used: 5,
	limit: 1000,
	remaining: 995,
	warning: null,
};
