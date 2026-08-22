import type { PendingCalibrationRow } from './tauri/types';

export interface CalibrationBatch {
	batch_id: string;
	computed_at: string;
	total_resolved: number;
	dims: PendingCalibrationRow[];
	/** True when the backend will refuse this batch — see `pulse_weights::stale_dimensions`. */
	superseded: boolean;
}

/**
 * Group pending calibration rows into batches and separate the ones that can
 * still be applied from the ones the weight guard refuses.
 *
 * Two rules that are easy to get wrong and are pinned by tests:
 *
 * 1. A batch is superseded if ANY dimension is stale, not if every one is.
 *    `apply_pending_calibration` refuses the whole batch on a single stale
 *    dimension, so an `every`-shaped check would render an Apply button that
 *    the backend then rejects.
 * 2. Actionable batches sort ahead of superseded ones regardless of date.
 *    After the 2026-08-22 reweight there are 18 superseded batches and one
 *    applicable one; newest-first alone buries the only card worth clicking.
 */
export function splitCalibrationBatches(rows: PendingCalibrationRow[]): {
	all: CalibrationBatch[];
	actionable: CalibrationBatch[];
	superseded: CalibrationBatch[];
} {
	const byBatch = new Map<string, PendingCalibrationRow[]>();
	for (const r of rows) {
		const dims = byBatch.get(r.batch_id);
		if (dims) dims.push(r);
		else byBatch.set(r.batch_id, [r]);
	}

	const batches = Array.from(byBatch.entries())
		.map(([batch_id, dims]) => ({
			batch_id,
			computed_at: dims[0].computed_at,
			total_resolved: dims[0].total_resolved,
			dims,
			superseded: dims.some((d) => d.stale_reason)
		}))
		.sort((a, b) => b.computed_at.localeCompare(a.computed_at));

	const actionable = batches.filter((b) => !b.superseded);
	const superseded = batches.filter((b) => b.superseded);
	return { all: [...actionable, ...superseded], actionable, superseded };
}
