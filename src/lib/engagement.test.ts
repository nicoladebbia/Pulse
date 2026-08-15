import { describe, it, expect } from 'vitest';
import {
	clampDwell,
	buildDetail,
	surfaceFromRoute,
	track,
	trackChatMessage,
	DETAIL_MAX
} from './engagement';

/**
 * The instrumentation's two hard requirements are that it cannot break a render
 * and cannot record content. Both are tested here rather than assumed.
 */

describe('track outside the app', () => {
	it('is a no-op instead of throwing when there is no window', () => {
		// vitest runs in node: `window` is undefined. isTauri() dereferences it, so a
		// missing guard here is not a silent no-op — it is a crash on every page.
		expect(() => track({ surface: '/', event: 'surface_view' })).not.toThrow();
	});

	it('does not throw for any of the convenience wrappers either', () => {
		expect(() => trackChatMessage(42)).not.toThrow();
	});
});

describe('clampDwell', () => {
	it('rounds to whole milliseconds', () => {
		expect(clampDwell(1234.6)).toBe(1235);
	});

	it('floors a negative to zero rather than letting Rust reject the event', () => {
		// A clock adjustment or a sleep/resume can produce this. Rust rejects a
		// negative dwell, and safeInvoke swallows the error — so an unclamped
		// negative loses the event silently.
		expect(clampDwell(-500)).toBe(0);
	});

	it('survives NaN and Infinity', () => {
		expect(clampDwell(NaN)).toBe(0);
		expect(clampDwell(Infinity)).toBe(0);
	});

	it('keeps a zero-length dwell as a real value', () => {
		expect(clampDwell(0)).toBe(0);
	});
});

describe('buildDetail', () => {
	it('serializes counters', () => {
		expect(buildDetail({ len: 42 })).toBe('{"len":42}');
	});

	it('serializes booleans', () => {
		expect(buildDetail({ filing: true })).toBe('{"filing":true}');
	});

	it('returns null rather than truncating something content-shaped', () => {
		// Truncation would produce half a headline in the DB, which is worse than
		// nothing: it looks like a legitimate value.
		const huge: Record<string, number> = {};
		for (let i = 0; i < 50; i++) huge['field_number_' + i] = i;
		expect(buildDetail(huge)).toBeNull();
	});

	it('accepts a payload exactly at the cap the Rust side enforces', () => {
		const detail = buildDetail({ len: 1 });
		expect(detail!.length).toBeLessThanOrEqual(DETAIL_MAX);
	});
});

describe('surfaceFromRoute', () => {
	it('passes a route id through as the surface name', () => {
		expect(surfaceFromRoute('/archive')).toBe('/archive');
	});

	it('keeps parameterized routes distinct from their parent', () => {
		// /freedoms and /freedoms/[freedom] are different surfaces: producing a
		// freedoms briefing is not the same as opening one.
		expect(surfaceFromRoute('/freedoms/[freedom]')).toBe('/freedoms/[freedom]');
	});

	it('falls back to the root for a null route id', () => {
		expect(surfaceFromRoute(null)).toBe('/');
		expect(surfaceFromRoute(undefined)).toBe('/');
		expect(surfaceFromRoute('')).toBe('/');
	});
});
