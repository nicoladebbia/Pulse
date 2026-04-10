import { writable, get } from 'svelte/store';
import type { FetchStatus } from '$lib/tauri/types';
import { isTauri } from '$lib/tauri/mock';

// All 10 pipeline stages in order
export const FETCH_STAGES = [
	{ id: 'collecting', label: 'Collecting sources' },
	{ id: 'deduplicating', label: 'Deduplicating articles' },
	{ id: 'summarizing', label: 'Summarizing stories' },
	{ id: 'analyzing', label: 'Cross-sector analysis' },
	{ id: 'executive_summary', label: 'Executive summary' },
	{ id: 'contextual', label: 'Contextual prefixes' },
	{ id: 'embeddings', label: 'Generating embeddings' },
	{ id: 'writing_db', label: 'Writing to database' },
	{ id: 'entities', label: 'Extracting entities' },
	{ id: 'deep_summaries', label: 'Deep analysis' },
] as const;

export type StageStatus = 'pending' | 'active' | 'completed' | 'failed';

export interface StageState {
	id: string;
	label: string;
	status: StageStatus;
	detail: string | null;
	percent: number | null; // sub-progress within this stage
}

// --- Stores ---
export const isFetching = writable(false);
export const fetchProgress = writable<FetchStatus | null>(null);
export const fetchDone = writable(false);
export const fetchError = writable<string | null>(null);
export const stages = writable<StageState[]>(
	FETCH_STAGES.map(s => ({ id: s.id, label: s.label, status: 'pending' as StageStatus, detail: null, percent: null }))
);

let pollInterval: ReturnType<typeof setInterval> | null = null;
let pollTimeout: ReturnType<typeof setTimeout> | null = null;

function resetStages() {
	highestSeenStage = -1;
	animationQueue = [];
	animating = false;
	stages.set(
		FETCH_STAGES.map(s => ({ id: s.id, label: s.label, status: 'pending', detail: null, percent: null }))
	);
}

// Track the highest stage index we've seen so we can animate one at a time
let highestSeenStage = -1;
let animationQueue: number[] = [];
let animating = false;

function processAnimationQueue() {
	if (animating || animationQueue.length === 0) return;
	animating = true;

	const nextIdx = animationQueue.shift()!;

	// Mark this stage as completed with a small delay for visual effect
	stages.update(prev =>
		prev.map((s, i) => i === nextIdx ? { ...s, status: 'completed' as StageStatus, detail: null, percent: null } : s)
	);

	setTimeout(() => {
		animating = false;
		processAnimationQueue();
	}, 150); // 150ms between each completion tick
}

function updateStagesFromProgress(status: FetchStatus) {
	if (!status.stage) return;

	const currentStageIdx = FETCH_STAGES.findIndex(s => s.id === status.stage);
	if (currentStageIdx === -1) return;

	// Queue completion animations for stages we skipped
	for (let i = highestSeenStage + 1; i < currentStageIdx; i++) {
		if (!animationQueue.includes(i)) {
			animationQueue.push(i);
		}
	}
	highestSeenStage = Math.max(highestSeenStage, currentStageIdx);

	// Process any queued completions
	processAnimationQueue();

	// Set current stage as active, future stages as pending
	stages.update(prev => {
		return prev.map((stage, i) => {
			if (i < currentStageIdx && stage.status === 'completed') {
				// Already animated to completed, keep it
				return stage;
			} else if (i === currentStageIdx) {
				return {
					...stage,
					status: 'active' as StageStatus,
					detail: status.detail,
					percent: status.percent,
				};
			} else if (i > currentStageIdx) {
				return { ...stage, status: 'pending' as StageStatus, detail: null, percent: null };
			}
			return stage;
		});
	});
}

function markAllCompleted() {
	stages.update(prev =>
		prev.map(s => ({ ...s, status: 'completed' as StageStatus, detail: null, percent: null }))
	);
}

export function stopPolling() {
	if (pollInterval) { clearInterval(pollInterval); pollInterval = null; }
	if (pollTimeout) { clearTimeout(pollTimeout); pollTimeout = null; }
}

export async function triggerFetch() {
	if (get(isFetching)) return;

	isFetching.set(true);
	fetchDone.set(false);
	fetchError.set(null);
	resetStages();

	try {
		if (!isTauri()) {
			// Mock: simulate stages
			simulateMockFetch();
			return;
		}

		const ipc = (window as any).__TAURI_INTERNALS__;
		try {
			await ipc.invoke('trigger_manual_fetch');
		} catch (e: any) {
			// "Fetch already in progress" is fine — we'll just join the existing fetch by polling
			const msg = String(e?.message ?? e);
			if (!msg.includes('already')) {
				throw e;
			}
			console.info('Joining existing fetch in progress');
		}

		// Wait 1s before polling so the fetcher has time to create progress file
		await new Promise(r => setTimeout(r, 1000));

		// Start polling for progress
		pollInterval = setInterval(async () => {
			try {
				const status: FetchStatus = await ipc.invoke('get_fetch_status');
				fetchProgress.set(status);
				updateStagesFromProgress(status);

				if (status.stage === 'failed') {
					stopPolling();
					isFetching.set(false);
					fetchError.set('Fetch failed');
					return;
				}

				if (!status.running) {
					stopPolling();
					markAllCompleted();
					fetchDone.set(true);
					isFetching.set(false);
				}
			} catch (e) {
				console.error('Polling error:', e);
			}
		}, 2000);

		// 15-minute timeout (large fetches with 150+ stories take 10+ minutes)
		pollTimeout = setTimeout(() => {
			stopPolling();
			isFetching.set(false);
			fetchError.set('Fetch timed out after 15 minutes');
		}, 900000);
	} catch (e: any) {
		stopPolling();
		isFetching.set(false);
		fetchError.set(String(e?.message ?? e));
	}
}

// Mock fetch simulation for browser dev
function simulateMockFetch() {
	let stageIdx = 0;
	const advance = () => {
		if (stageIdx >= FETCH_STAGES.length) {
			markAllCompleted();
			fetchDone.set(true);
			setTimeout(() => isFetching.set(false), 3000);
			return;
		}

		stages.update(prev =>
			prev.map((s, i) => {
				if (i < stageIdx) return { ...s, status: 'completed' as StageStatus, detail: null, percent: null };
				if (i === stageIdx) return { ...s, status: 'active' as StageStatus, detail: 'Processing...', percent: 50 };
				return { ...s, status: 'pending' as StageStatus, detail: null, percent: null };
			})
		);

		stageIdx++;
		setTimeout(advance, 800 + Math.random() * 400);
	};
	advance();
}
