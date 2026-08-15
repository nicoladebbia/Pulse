/**
 * Local-only engagement instrumentation.
 *
 * Pulse records what it produces and nothing it consumes, so "is this surface
 * used?" had no answer — `backtest_results` writes one row a day whether or not
 * anyone opens Trading. This module records the other half.
 *
 * Three rules hold everywhere below:
 *  - It never throws and never blocks. Every call is fire-and-forget through
 *    `safeInvoke`, which already swallows and logs. A render path must not be
 *    able to fail because a counter did.
 *  - It never stores user text. Message bodies, queries and headlines stay out;
 *    `detail` carries counters like {"len":42}. The Rust side enforces this with
 *    a length cap, so a leak fails loudly in tests rather than landing in the DB.
 *  - Nothing leaves the machine. There is no network path out of this file.
 */

import { invoke } from '@tauri-apps/api/core';
import { isTauri } from '$lib/tauri/mock';
import type { Story } from '$lib/tauri/types';

export type EngagementEvent =
	| 'surface_view'
	| 'story_open'
	| 'story_close'
	| 'sector_filter'
	| 'chat_message'
	| 'citation_click';

export interface EngagementPayload {
	surface: string;
	event: EngagementEvent;
	storyId?: number | null;
	briefingId?: number | null;
	sector?: string | null;
	dwellMs?: number | null;
	detail?: string | null;
}

/** Paired with MAX_FIELD_LEN in src-tauri/src/commands/engagement.rs. */
export const DETAIL_MAX = 128;

/**
 * A dwell is a wall-clock difference, so a clock adjustment or a resumed-from-sleep
 * window can hand back a negative or absurd number. The Rust side rejects negatives
 * outright, which would silently drop the event; clamp here instead.
 */
export function clampDwell(ms: number): number {
	if (!Number.isFinite(ms) || ms < 0) return 0;
	return Math.round(ms);
}

/**
 * Counters only. Returns null rather than a truncated string if the caller ever
 * passes something content-shaped — a half-headline is worse than no detail.
 */
export function buildDetail(fields: Record<string, number | boolean>): string | null {
	const json = JSON.stringify(fields);
	return json.length <= DETAIL_MAX ? json : null;
}

/** SvelteKit route ids are the surface names, so a new route needs no registration. */
export function surfaceFromRoute(routeId: string | null | undefined): string {
	return routeId && routeId.length > 0 ? routeId : '/';
}

function inApp(): boolean {
	// isTauri() reads `window`, which does not exist under vitest or SSR.
	return typeof window !== 'undefined' && isTauri();
}

export function track(payload: EngagementPayload): void {
	if (!inApp()) return;
	// Deliberately not `safeInvoke`: commands.ts imports this module to count chat
	// messages, and importing it back would make the two files a cycle. The error
	// handling safeInvoke provides is reproduced here instead — a dropped counter
	// must never surface anywhere near a render path.
	try {
		void invoke('record_engagement', { event: payload }).catch((err) => {
			console.warn('[engagement] dropped', err);
		});
	} catch (err) {
		console.warn('[engagement] dropped', err);
	}
}

export function trackSurfaceView(routeId: string | null | undefined): void {
	track({ surface: surfaceFromRoute(routeId), event: 'surface_view' });
}

export function trackStoryOpen(story: Story, routeId: string | null | undefined): void {
	track({
		surface: surfaceFromRoute(routeId),
		event: 'story_open',
		storyId: story.id,
		briefingId: story.briefing_id,
		sector: story.sector,
		detail: buildDetail({ filing: story.source_type === 'financial' })
	});
}

export function trackStoryClose(
	story: Story,
	routeId: string | null | undefined,
	dwellMs: number
): void {
	track({
		surface: surfaceFromRoute(routeId),
		event: 'story_close',
		storyId: story.id,
		briefingId: story.briefing_id,
		sector: story.sector,
		dwellMs: clampDwell(dwellMs)
	});
}

/**
 * `toggled` is null when every filter was cleared at once. That is a widening, not a
 * toggle of one sector, and naming a fake sector for it would put a phantom row in
 * the per-sector rollup that `get_engagement_summary` groups.
 */
export function trackSectorFilter(
	routeId: string | null | undefined,
	toggled: string | null,
	activeCount: number
): void {
	track({
		surface: surfaceFromRoute(routeId),
		event: 'sector_filter',
		sector: toggled,
		detail: buildDetail({ active: activeCount, cleared: toggled === null })
	});
}

/** Length only — the message itself is never recorded. */
export function trackChatMessage(messageLength: number): void {
	track({
		surface: '/ask',
		event: 'chat_message',
		detail: buildDetail({ len: messageLength })
	});
}

export function trackCitationClick(routeId: string | null | undefined, storyId: number): void {
	track({
		surface: surfaceFromRoute(routeId),
		event: 'citation_click',
		storyId
	});
}
