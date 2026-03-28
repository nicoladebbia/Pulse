import { invoke } from '@tauri-apps/api/core';
import type { BriefingWithStories, Story, StoryDetail } from './types';

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

export async function fullTextSearch(query: string): Promise<Story[]> {
	return invoke('full_text_search', { query });
}

export async function triggerManualFetch(): Promise<string> {
	return invoke('trigger_manual_fetch');
}

export async function getFetchStatus(): Promise<boolean> {
	return invoke('get_fetch_status');
}
