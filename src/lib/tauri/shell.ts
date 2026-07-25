// Opening an external URL was the app's only Tauri invoke living outside a try/catch,
// duplicated across three components. A floating rejected invoke is an unhandled
// rejection, which takes down the webview — so the catch lives here, once, where a
// fourth call site can't forget it.
//
// Deliberately uses the __TAURI_INTERNALS__ escape hatch instead of @tauri-apps/api:
// importing that at Svelte module scope kills the page, and it lets the non-Tauri
// (browser dev / mock) path fall back to window.open.
export async function openExternal(url: string): Promise<void> {
	try {
		const ipc = (window as any).__TAURI_INTERNALS__;
		if (!ipc) {
			window.open(url, '_blank');
			return;
		}
		await ipc.invoke('plugin:shell|open', { path: url });
	} catch (err) {
		console.error('[openExternal failed]', url, err);
	}
}
