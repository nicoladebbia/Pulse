import { writable, get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
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
export const fetchEta = writable<{ eta_secs: number | null; elapsed_secs: number | null }>({ eta_secs: null, elapsed_secs: null });

// Always-visible last-run state (driven by the global poller, works for launchd runs too).
export const lastStatus = writable<FetchStatus['last_status']>('idle');
export const lastReason = writable<string | null>(null);
export const lastAt = writable<string | null>(null);

// Set true optimistically on a manual click so the UI reacts before the fetcher process
// has booted and written its first progress file (it runs form4/enrich for a few seconds
// first). OR'd with the freshness-derived running signal from the poller.
let optimisticUntil = 0;

// Smoothed ETA: only allow increases up to 15% of previous value to prevent bouncing
let lastSmoothedEta: number | null = null;
export const stages = writable<StageState[]>(
	FETCH_STAGES.map(s => ({ id: s.id, label: s.label, status: 'pending' as StageStatus, detail: null, percent: null }))
);

let pollInterval: ReturnType<typeof setInterval> | null = null;

function resetStages() {
	highestSeenStage = -1;
	animationQueue = [];
	animating = false;
	lastSmoothedEta = null;
	fetchEta.set({ eta_secs: null, elapsed_secs: null });
	stages.set(
		FETCH_STAGES.map(s => ({ id: s.id, label: s.label, status: 'pending', detail: null, percent: null }))
	);
}

function updateSmoothedEta(rawEta: number | null, elapsed: number | null) {
	if (rawEta == null) {
		fetchEta.set({ eta_secs: lastSmoothedEta, elapsed_secs: elapsed });
		return;
	}
	if (lastSmoothedEta == null) {
		lastSmoothedEta = rawEta;
	} else {
		// Allow decreases freely, but cap increases at 15% of previous
		const maxAllowed = lastSmoothedEta * 1.15;
		lastSmoothedEta = Math.min(rawEta, maxAllowed);
	}
	fetchEta.set({ eta_secs: Math.round(lastSmoothedEta), elapsed_secs: elapsed });
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

// Whether the previous applied status had a fetch running — so we can detect the
// running → not-running EDGE and fire the completion animation exactly once, even for
// launchd/scheduled runs the app never triggered.
let wasRunning = false;

/** Apply a FetchStatus snapshot to every store. The single place status → UI happens. */
function applyStatus(status: FetchStatus) {
	fetchProgress.set(status);
	lastStatus.set(status.last_status);
	lastReason.set(status.last_reason ?? null);
	lastAt.set(status.last_at ?? null);

	// Once the backend confirms a real running fetch, drop the optimistic window — the
	// file is now authoritative (so a genuine mid-run failure isn't masked by the grace).
	if (status.running) optimisticUntil = 0;

	// running is true if the backend sees a fresh in-progress file OR we're still inside
	// the optimistic window right after a manual click (fetcher not booted yet).
	const running = status.running || Date.now() < optimisticUntil;
	isFetching.set(running);

	if (running) {
		fetchDone.set(false);
		fetchError.set(null);
		updateStagesFromProgress(status);
		updateSmoothedEta(status.eta_secs ?? null, status.elapsed_secs ?? null);
		wasRunning = true;
		return;
	}

	// Not running. Surface a terminal failure/interruption regardless of who started it.
	if (status.last_status === 'failed' || status.last_status === 'interrupted') {
		fetchError.set(status.last_reason ?? (status.last_status === 'interrupted' ? 'Fetch interrupted' : 'Fetch failed'));
	} else {
		fetchError.set(null);
	}

	// Fire the completion animation on the running → done edge (a fetch just finished).
	if (wasRunning) {
		if (status.last_status === 'complete') {
			markAllCompleted();
			fetchDone.set(true);
		}
		wasRunning = false;
	}
}

/** Poll get_fetch_status once and apply it. Safe to call from the global poller. */
async function pollOnce() {
	// NOTE: do NOT gate on isTauri() here. The official `invoke` from @tauri-apps/api/core
	// works in the Tauri webview regardless of whether window.__TAURI_INTERNALS__ is already
	// populated at call time; gating on isTauri() was silently no-op'ing the poller. In a
	// plain browser (mock dev) invoke() throws and is swallowed by the catch — harmless.
	try {
		const status = await invoke<FetchStatus>('get_fetch_status');
		applyStatus(status);
	} catch (e) {
		// Expected in non-Tauri (browser dev) — invoke isn't available there.
	}
}

/**
 * Start the ALWAYS-ON status poller. Mounted once at app load (in +layout.svelte) so the
 * progress bar reflects ANY fetch — manual, scheduled launchd run, or a crash that happened
 * while the app was closed — without the user clicking anything.
 */
export function startGlobalPolling(intervalMs = 1000) {
	if (pollInterval) return; // already running
	pollOnce(); // immediate first read so the bar isn't blank for a second
	pollInterval = setInterval(pollOnce, intervalMs);
}

export function stopPolling() {
	if (pollInterval) { clearInterval(pollInterval); pollInterval = null; }
}

export async function triggerFetch() {
	if (get(isFetching)) return;

	// Optimistic: light the UI immediately (covers the seconds before the fetcher boots).
	optimisticUntil = Date.now() + 20000; // 20s cold-start grace; the poller takes over once the file appears
	isFetching.set(true);
	fetchDone.set(false);
	fetchError.set(null);
	resetStages();
	wasRunning = true;

	try {
		if (!isTauri()) {
			// Mock: simulate stages
			simulateMockFetch();
			return;
		}

		try {
			await invoke('trigger_manual_fetch');
		} catch (e: any) {
			// "Fetch already in progress" is fine — the global poller will track it.
			const msg = String(e?.message ?? e);
			if (!msg.includes('already')) {
				throw e;
			}
			console.info('Joining existing fetch in progress');
		}

		// Ensure the global poller is running (idempotent). It drives all UI from here.
		startGlobalPolling();
	} catch (e: any) {
		optimisticUntil = 0;
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
