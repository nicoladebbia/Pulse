import { describe, it, expect } from 'vitest';
import { splitCalibrationBatches } from './calibration';
import type { PendingCalibrationRow } from './tauri/types';

function row(
	batch_id: string,
	dimension: string,
	computed_at: string,
	stale_reason?: string | null
): PendingCalibrationRow {
	return {
		id: 0,
		batch_id,
		computed_at,
		dimension,
		old_weight: 0.25,
		new_weight: 0.3,
		hit_rate: 0.6,
		sample_size: 10,
		total_resolved: 30,
		status: 'pending',
		stale_reason
	};
}

describe('splitCalibrationBatches', () => {
	it('groups rows by batch and keeps the batch metadata', () => {
		const { all } = splitCalibrationBatches([
			row('b1', 'insider_signal', '2026-08-20'),
			row('b1', 'news_momentum', '2026-08-20')
		]);
		expect(all).toHaveLength(1);
		expect(all[0].dims.map((d) => d.dimension)).toEqual(['insider_signal', 'news_momentum']);
		expect(all[0].computed_at).toBe('2026-08-20');
		expect(all[0].total_resolved).toBe(30);
	});

	// The bug this exists to prevent: a batch is superseded if ANY dimension is
	// stale, not if every one is. `apply_pending_calibration` refuses the WHOLE
	// batch on one stale dimension, so an `every`-shaped check would offer an
	// Apply button that the backend then rejects.
	it('treats a batch with ONE stale dimension as superseded', () => {
		const { actionable, superseded } = splitCalibrationBatches([
			row('b1', 'insider_signal', '2026-08-20'),
			row('b1', 'news_momentum', '2026-08-20', 'old_weight 0.2391 is no longer a live weight')
		]);
		expect(superseded.map((b) => b.batch_id)).toEqual(['b1']);
		expect(actionable).toEqual([]);
	});

	it('counts a batch with no stale dimension as actionable', () => {
		const { actionable, superseded } = splitCalibrationBatches([
			row('b1', 'insider_signal', '2026-08-20'),
			row('b1', 'news_momentum', '2026-08-20', null)
		]);
		expect(actionable.map((b) => b.batch_id)).toEqual(['b1']);
		expect(superseded).toEqual([]);
	});

	// The whole point of the split: one applicable batch must not be buried
	// under 18 dead ones just because the dead ones are newer.
	it('orders every actionable batch ahead of every superseded one', () => {
		const rows = [
			row('stale-newest', 'insider_signal', '2026-08-21', 'superseded'),
			row('live-older', 'insider_signal', '2026-08-10'),
			row('stale-older', 'insider_signal', '2026-08-05', 'superseded')
		];
		expect(splitCalibrationBatches(rows).all.map((b) => b.batch_id)).toEqual([
			'live-older',
			'stale-newest',
			'stale-older'
		]);
	});

	it('sorts newest first within each group', () => {
		const rows = [
			row('a', 'insider_signal', '2026-08-01', 'superseded'),
			row('b', 'insider_signal', '2026-08-19', 'superseded'),
			row('c', 'insider_signal', '2026-08-10', 'superseded')
		];
		expect(splitCalibrationBatches(rows).superseded.map((b) => b.batch_id)).toEqual([
			'b',
			'c',
			'a'
		]);
	});

	it('survives an empty list', () => {
		expect(splitCalibrationBatches([])).toEqual({ all: [], actionable: [], superseded: [] });
	});
});
