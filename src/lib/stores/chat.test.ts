import { describe, it, expect } from 'vitest';
import { nextStreamGeneration, invalidateStreams, isStaleGeneration } from './chat';

describe('stream generation', () => {
	it('accepts events from the stream that is currently running', () => {
		const mine = nextStreamGeneration();
		expect(isStaleGeneration(mine)).toBe(false);
	});

	it('discards events from a stream abandoned by a thread switch', () => {
		const mine = nextStreamGeneration();
		invalidateStreams();
		expect(isStaleGeneration(mine)).toBe(true);
	});

	// The case a naive "has anything changed since I started?" check breaks: the
	// NEWER stream must keep working while the older one stays discarded. Both
	// conditions have to hold at the same moment, which is why this is one test.
	it('keeps the newer stream live while the older one stays discarded', () => {
		const older = nextStreamGeneration();
		const newer = nextStreamGeneration();
		expect(isStaleGeneration(newer)).toBe(false);
		expect(isStaleGeneration(older)).toBe(true);
	});

	// Generations must never be reused. If the counter reset or wrapped, an
	// abandoned stream could become "current" again and resume writing into a
	// thread the user had already left.
	it('never reissues a generation, so an abandoned stream cannot revive', () => {
		const seen = new Set<number>();
		for (let i = 0; i < 50; i++) {
			const g = nextStreamGeneration();
			expect(seen.has(g)).toBe(false);
			seen.add(g);
			invalidateStreams();
			expect(isStaleGeneration(g)).toBe(true);
		}
		expect(seen.size).toBe(50);
	});
});
