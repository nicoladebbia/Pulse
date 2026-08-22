export const SECTORS = {
	ai: { id: 'ai', name: 'AI & LLMs', color: 'var(--color-ai)', dimColor: 'var(--color-ai-dim)', key: '1' },
	miami: { id: 'miami', name: 'Miami Beach', color: 'var(--color-miami)', dimColor: 'var(--color-miami-dim)', key: '2' },
	italy: { id: 'italy', name: 'Italy', color: 'var(--color-italy)', dimColor: 'var(--color-italy-dim)', key: '3' },
	tech: { id: 'tech', name: 'Tech & Innovation', color: 'var(--color-tech)', dimColor: 'var(--color-tech-dim)', key: '4' },
} as const;

export type SectorId = keyof typeof SECTORS;

export const SECTOR_ORDER: SectorId[] = ['ai', 'miami', 'italy', 'tech'];

// Freedom configurations
export const FREEDOM_CONFIG = {
	time: {
		id: 'time', key: 'time_stories' as const,
		label: 'Time Freedom', subtitle: 'Reclaim your hours',
		icon: '⏳', color: 'var(--color-freedom-time)', dim: 'var(--color-freedom-time-dim)',
		tagline: 'Productivity, automation, passive income, delegation',
		description: 'Intelligence on productivity, automation, passive income, AI delegation, async work, and the 4-day work week movement.',
		motif: 'linear-gradient(180deg, var(--color-freedom-time-dim) 0%, transparent 40%)',
	},
	wealth: {
		id: 'wealth', key: 'wealth_stories' as const,
		label: 'Wealth Freedom', subtitle: 'Build your wealth',
		icon: '◆', color: 'var(--color-freedom-financial)', dim: 'var(--color-freedom-financial-dim)',
		tagline: 'Investing, markets, crypto, FIRE, passive income, real estate',
		description: 'Intelligence on investing, markets, crypto, real estate, startup funding, bootstrapping, FIRE, tax strategies, and wealth building.',
		motif: 'linear-gradient(180deg, var(--color-freedom-financial-dim) 0%, transparent 40%)',
	},
	location: {
		id: 'location', key: 'location_stories' as const,
		label: 'Location Freedom', subtitle: 'Work from anywhere',
		icon: '◎', color: 'var(--color-freedom-location)', dim: 'var(--color-freedom-location-dim)',
		tagline: 'Remote work, digital nomad, visas, global mobility',
		description: 'Intelligence on remote work, digital nomad life, visa programs, relocation, cost of living, and global mobility.',
		motif: 'linear-gradient(180deg, var(--color-freedom-location-dim) 0%, transparent 40%)',
	},
	health: {
		id: 'health', key: 'health_stories' as const,
		label: 'Health Freedom', subtitle: 'Optimize your body',
		icon: '♥', color: 'var(--color-freedom-health)', dim: 'var(--color-freedom-health-dim)',
		tagline: 'Longevity, biohacking, fitness tech, nutrition, sleep',
		description: 'Intelligence on longevity, biohacking, fitness tech, nutrition, mental health, sleep science, and wearables.',
		motif: 'linear-gradient(180deg, var(--color-freedom-health-dim) 0%, transparent 40%)',
	},
	whoop: {
		id: 'whoop', key: 'whoop_stories' as const,
		label: 'Whoop', subtitle: 'Your wearable',
		icon: '◐', color: 'var(--color-freedom-whoop)', dim: 'var(--color-freedom-whoop-dim)',
		tagline: 'Whoop product, app, data, HRV, recovery science',
		description: 'Intelligence on Whoop hardware, app updates, features, HRV and recovery research, and the wearable ecosystem.',
		motif: 'linear-gradient(180deg, var(--color-freedom-whoop-dim) 0%, transparent 40%)',
	},
} as const;

export type FreedomId = keyof typeof FREEDOM_CONFIG;
export const FREEDOM_ORDER: FreedomId[] = ['time', 'wealth', 'location', 'health', 'whoop'];
