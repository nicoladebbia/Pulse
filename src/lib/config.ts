export const SECTORS = {
	ai: { id: 'ai', name: 'AI & LLMs', color: 'var(--color-ai)', dimColor: 'var(--color-ai-dim)', key: '1' },
	miami: { id: 'miami', name: 'Miami Beach', color: 'var(--color-miami)', dimColor: 'var(--color-miami-dim)', key: '2' },
	italy: { id: 'italy', name: 'Italian Politics', color: 'var(--color-italy)', dimColor: 'var(--color-italy-dim)', key: '3' },
	tech: { id: 'tech', name: 'Tech & Innovation', color: 'var(--color-tech)', dimColor: 'var(--color-tech-dim)', key: '4' },
} as const;

export type SectorId = keyof typeof SECTORS;

export const SECTOR_ORDER: SectorId[] = ['ai', 'miami', 'italy', 'tech'];
