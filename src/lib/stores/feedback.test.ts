import { describe, it, expect, beforeEach, vi } from 'vitest';
import { get } from 'svelte/store';
import { feedbackLabels, nextLabel, toggleFeedback, requestFeedback, _setBridge } from './feedback';

/**
 * The store is the only thing between a tap and the ground-truth table, so the
 * properties that matter are tested: what the control shows is what was stored,
 * and a briefing's worth of controls costs one lookup, not one per card.
 */

const tick = () => new Promise((r) => setTimeout(r, 0));

describe('nextLabel', () => {
	it('sets, switches, and clears on a second tap', () => {
		expect(nextLabel(null, 'mattered')).toBe('mattered');
		expect(nextLabel('mattered', 'didnt')).toBe('didnt');
		expect(nextLabel('didnt', 'didnt')).toBeNull();
	});
});

describe('toggleFeedback', () => {
	let calls: [string, Record<string, unknown>][];

	beforeEach(() => {
		calls = [];
	});

	it('writes the new label and a null on clear', async () => {
		_setBridge(async (cmd, args) => {
			calls.push([cmd, args]);
		});
		await toggleFeedback(7, 'mattered');
		expect(get(feedbackLabels)[7]).toBe('mattered');
		await toggleFeedback(7, 'mattered');
		expect(get(feedbackLabels)[7]).toBeUndefined();
		expect(calls).toEqual([
			['set_story_feedback', { storyId: 7, label: 'mattered' }],
			['set_story_feedback', { storyId: 7, label: null }]
		]);
	});

	it('reverts to the previous label when the write fails', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		_setBridge(async () => undefined);
		await toggleFeedback(3, 'didnt');
		_setBridge(async () => {
			throw new Error('db locked');
		});
		// _setBridge resets the store; restore the saved state the UI would have had.
		feedbackLabels.set({ 3: 'didnt' });
		await toggleFeedback(3, 'mattered');
		expect(get(feedbackLabels)[3]).toBe('didnt');
		warn.mockRestore();
	});

	it('keeps the label locally outside the app instead of failing', async () => {
		const bridge = vi.fn();
		_setBridge(bridge, () => false);
		await toggleFeedback(9, 'mattered');
		expect(get(feedbackLabels)[9]).toBe('mattered');
		expect(bridge).not.toHaveBeenCalled();
	});
});

describe('requestFeedback', () => {
	it('batches every ask in one tick into a single lookup, once per story', async () => {
		const bridge = vi.fn(async () => [{ story_id: 2, label: 'mattered' }]);
		_setBridge(bridge);
		requestFeedback(1);
		requestFeedback(2);
		requestFeedback(2);
		await tick();
		requestFeedback(1);
		await tick();
		expect(bridge).toHaveBeenCalledTimes(1);
		expect(bridge).toHaveBeenCalledWith('get_story_feedback', { storyIds: [1, 2] });
		expect(get(feedbackLabels)).toEqual({ 2: 'mattered' });
	});

	it('does not let a stale lookup overwrite a tap made while it was in flight', async () => {
		let release: (v: unknown) => void = () => {};
		const bridge = vi.fn((cmd: string) =>
			cmd === 'get_story_feedback' ? new Promise((r) => (release = r)) : Promise.resolve()
		);
		_setBridge(bridge);
		requestFeedback(5);
		await tick();
		await toggleFeedback(5, 'didnt');
		release([{ story_id: 5, label: 'mattered' }]);
		await tick();
		expect(get(feedbackLabels)[5]).toBe('didnt');
	});
});
