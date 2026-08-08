#!/usr/bin/env node
// Drive the Pulse frontend in headless Chrome over the DevTools Protocol.
//
// Zero dependencies: Node >=22 ships a global WebSocket, so no `ws` package is
// needed. Launches its own Chrome on a free port, drives it, tears it down.
//
//   node .claude/skills/run-pulse/cdp.mjs --url http://localhost:5173/trends \
//     --out /tmp/trends.png --click 'button.block' --eval 'document.title'
//
// Flags:
//   --url <url>          page to open (required)
//   --out <path.png>     write a full-page screenshot
//   --click <selector>   click an element's centre-top; repeatable, in order
//   --eval <expr>        evaluate JS and print the result; repeatable
//   --wait <ms>          settle time after load and after each click (default 900)
//   --viewport WxH       window size (default 1200x1400)
//   --timeout <ms>       overall budget before giving up (default 60000)
//
// Exit code is non-zero if the page logged a console error or threw.

import { spawn } from 'node:child_process';
import { writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import net from 'node:net';

const CHROME_CANDIDATES = [
	'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
	'/Applications/Brave Browser.app/Contents/MacOS/Brave Browser',
	'/Applications/Chromium.app/Contents/MacOS/Chromium',
	'/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge',
];

function parseArgs(argv) {
	// `steps` preserves the ORDER of --click/--eval as given. Running all clicks first
	// and all evals after would silently reorder a before/after measurement and report
	// a confident wrong answer.
	const o = { steps: [], wait: 900, viewport: '1200x1400', timeout: 60000 };
	for (let i = 0; i < argv.length; i++) {
		const a = argv[i];
		const next = () => argv[++i];
		if (a === '--url') o.url = next();
		else if (a === '--out') o.out = next();
		else if (a === '--click') o.steps.push({ type: 'click', value: next() });
		else if (a === '--eval') o.steps.push({ type: 'eval', value: next() });
		else if (a === '--ready') o.ready = next();
		else if (a === '--wait') o.wait = Number(next());
		else if (a === '--viewport') o.viewport = next();
		else if (a === '--timeout') o.timeout = Number(next());
		else throw new Error(`unknown flag: ${a}`);
	}
	if (!o.url) throw new Error('--url is required');
	return o;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const freePort = () =>
	new Promise((res, rej) => {
		const s = net.createServer();
		s.on('error', rej);
		s.listen(0, '127.0.0.1', () => {
			const { port } = s.address();
			s.close(() => res(port));
		});
	});

async function findChrome() {
	const { access } = await import('node:fs/promises');
	for (const c of CHROME_CANDIDATES) {
		try { await access(c); return c; } catch {}
	}
	throw new Error('no Chromium-family browser found in /Applications');
}

async function waitForCdp(port, deadline) {
	while (Date.now() < deadline) {
		try {
			const r = await fetch(`http://127.0.0.1:${port}/json`);
			if (r.ok) {
				const targets = await r.json();
				const page = targets.find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
				if (page) return page;
			}
		} catch {}
		await sleep(250);
	}
	throw new Error('Chrome CDP endpoint never became ready');
}

const opts = parseArgs(process.argv.slice(2));
const deadline = Date.now() + opts.timeout;
const [w, h] = opts.viewport.split('x').map(Number);
const chrome = await findChrome();
const port = await freePort();
const profile = mkdtempSync(join(tmpdir(), 'pulse-cdp-'));

const proc = spawn(chrome, [
	'--headless=new',
	'--disable-gpu',
	'--hide-scrollbars',
	'--no-first-run',
	'--no-default-browser-check',
	'--disable-extensions',
	`--remote-debugging-port=${port}`,
	`--user-data-dir=${profile}`,
	`--window-size=${w},${h}`,
	'about:blank',
], { stdio: 'ignore', detached: false });

const cleanup = () => {
	try { proc.kill('SIGKILL'); } catch {}
	try { rmSync(profile, { recursive: true, force: true }); } catch {}
};
process.on('exit', cleanup);
process.on('SIGINT', () => { cleanup(); process.exit(130); });

let failed = false;
try {
	const target = await waitForCdp(port, deadline);
	const ws = new WebSocket(target.webSocketDebuggerUrl);
	await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error('CDP socket failed')); });

	let id = 0;
	const pending = new Map();
	const events = [];
	ws.onmessage = (m) => {
		const msg = JSON.parse(m.data);
		if (msg.id && pending.has(msg.id)) {
			const { res, rej } = pending.get(msg.id);
			pending.delete(msg.id);
			msg.error ? rej(new Error(JSON.stringify(msg.error))) : res(msg.result);
		} else if (msg.method) events.push(msg);
	};
	const send = (method, params = {}) =>
		new Promise((res, rej) => {
			pending.set(++id, { res, rej });
			ws.send(JSON.stringify({ id, method, params }));
		});

	await send('Page.enable');
	await send('Runtime.enable');
	await send('Log.enable');

	await send('Page.navigate', { url: opts.url });
	await sleep(opts.wait);

	const evaluate = (expression) => send('Runtime.evaluate', { expression, returnByValue: true });

	// Poll for the element rather than trusting a fixed sleep. The SPA needs ~3s to route,
	// run its $effect and load data; a fixed wait was flaky and clicked into an empty page.
	async function locate(sel) {
		const expr = `(() => {
			const el = document.querySelector(${JSON.stringify(sel)});
			if (!el) return null;
			el.scrollIntoView({ block: 'center' });
			const r = el.getBoundingClientRect();
			if (r.width === 0 || r.height === 0) return null;
			// centre-x, near the top edge: safe for tall cards whose centre may be offscreen
			return JSON.stringify({ x: r.left + r.width / 2, y: r.top + Math.min(30, r.height / 2) });
		})()`;
		while (Date.now() < deadline) {
			const { result } = await evaluate(expr);
			if (result.value) return JSON.parse(result.value);
			await sleep(200);
		}
		const { result: what } = await evaluate(
			`[...document.querySelectorAll("button,a")].map(e => e.tagName + "." + String(e.className).slice(0,40)).slice(0,25)`,
		);
		throw new Error(`selector never appeared: ${sel}\n  clickable elements present: ${JSON.stringify(what.value)}`);
	}

	// Gate every step on the page actually being rendered. Without this a leading --eval
	// measures an empty page and returns a plausible-but-meaningless "before" value.
	if (opts.ready) {
		await locate(opts.ready);
		console.log(`ready: ${opts.ready}`);
	}

	for (const step of opts.steps) {
		if (step.type === 'click') {
			const { x, y } = await locate(step.value);
			await send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: 1 });
			await send('Input.dispatchMouseEvent', { type: 'mouseReleased', x, y, button: 'left', clickCount: 1 });
			console.log(`clicked: ${step.value}`);
			await sleep(opts.wait);
		} else {
			const { result, exceptionDetails } = await evaluate(step.value);
			if (exceptionDetails) {
				console.log(`eval threw: ${step.value} -> ${exceptionDetails.text}`);
				failed = true;
			} else {
				console.log(`eval: ${step.value} -> ${JSON.stringify(result.value)}`);
			}
		}
	}

	const bad = events.filter(
		(e) =>
			e.method === 'Runtime.exceptionThrown' ||
			(e.method === 'Log.entryAdded' && e.params?.entry?.level === 'error') ||
			(e.method === 'Runtime.consoleAPICalled' && e.params?.type === 'error'),
	);
	console.log(`console errors / exceptions: ${bad.length}`);
	for (const b of bad.slice(0, 10)) console.log('  -', JSON.stringify(b.params).slice(0, 300));
	if (bad.length) failed = true;

	if (opts.out) {
		const shot = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true });
		writeFileSync(opts.out, Buffer.from(shot.data, 'base64'));
		console.log(`wrote ${opts.out}`);
	}
	ws.close();
} finally {
	cleanup();
}
process.exit(failed ? 1 : 0);
