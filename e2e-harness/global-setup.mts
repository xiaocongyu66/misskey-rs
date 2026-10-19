// vitest globalSetup for running upstream Misskey e2e specs against the
// misskey-rs server. Mirrors packages/backend/test-server/{entry,server}.ts:
//   setup()   → serve the API on PORT (spawn misskey-rs) + controller endpoints on PORT+1000
//   teardown()→ stop both
// The upstream per-suite flow (`setup.e2e.ts`) then does env-reset +
// initTestDb (dropSchema+synchronize from entities) as usual.

import { spawn } from 'node:child_process';
import type { ChildProcess } from 'node:child_process';
import { createServer } from 'node:http';
import type { Server } from 'node:http';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const PORT = Number(process.env.E2E_PORT ?? 3000);
const BIN = process.env.MISSKEY_RS_BIN ?? 'misskey-rs';

let child: ChildProcess | undefined;
let controller: Server | undefined;

function dbDefaults() {
	// mirrors test/compose.yml / CI service env
	return {
		host: process.env.E2E_PG_HOST ?? '127.0.0.1',
		port: process.env.E2E_PG_PORT ?? '5432',
		user: process.env.E2E_PG_USER ?? 'postgres',
		pass: process.env.E2E_PG_PASS ?? 'postgres',
		db: process.env.E2E_PG_DB ?? 'misskey_test',
	};
}

function serverConfigYaml(): string {
	const pg = dbDefaults();
	// arena-oauth: suites that pin the RFC9207/8414 issuer to the upstream
	// test.yml url (oauth.ts asserts the literal 'http://misskey.local') get
	// it via MKRS_E2E_URL; everything else keeps the 127.0.0.1 default.
	const url = process.env.MKRS_E2E_URL ?? `http://127.0.0.1:${PORT}/`;
	return [
		`url: ${url}`,
		`port: ${PORT}`,
		'db:',
		`  host: ${pg.host}`,
		`  port: ${pg.port}`,
		`  user: ${pg.user}`,
		`  pass: ${pg.pass}`,
		`  db: ${pg.db}`,
		'redis:',
		'  host: 127.0.0.1',
		'  port: 6379',
		'test: true',
	].join('\n');
}

async function waitHealthy(url: string, timeoutMs = 60_000): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	while (Date.now() < deadline) {
		try {
			const res = await fetch(url);
			if (res.ok) return;
		} catch {
			// not up yet
		}
		await new Promise((r) => setTimeout(r, 500));
	}
	throw new Error(`server did not become healthy: ${url}`);
}

async function postReset(): Promise<void> {
	try {
		await fetch(`http://127.0.0.1:${PORT}/api/_test/reset`, { method: 'POST' });
	} catch (e) {
		console.error('[mkrs-harness] reset failed', e);
	}
}

export async function setup(): Promise<void> {
	const dir = mkdtempSync(join(tmpdir(), 'mkrs-e2e-'));
	const cfgPath = join(dir, 'config.yaml');
	writeFileSync(cfgPath, serverConfigYaml());
	console.log(`[mkrs-harness] spawning ${BIN} (config: ${cfgPath})`);

	child = spawn(BIN, [cfgPath], { stdio: 'inherit', env: process.env });
	child.on('exit', (code) => {
		if (code != null) console.error(`[mkrs-harness] server exited with ${code}`);
	});

	await waitHealthy(`http://127.0.0.1:${PORT}/healthz`);

	// controller endpoints on PORT+1000 (mirrors test-server/server.ts)
	controller = createServer((req, res) => {
		if (req.method === 'POST' && req.url?.startsWith('/env-reset')) {
			void postReset().then(() => {
				res.setHeader('content-type', 'application/json');
				res.end(JSON.stringify({ success: true }));
			});
			return;
		}
		if (req.method === 'POST' && req.url?.startsWith('/env')) {
			let body = '';
			req.on('data', (d) => (body += d));
			req.on('end', () => {
				void (async () => {
					try {
						const { key, value } = JSON.parse(body) as { key?: string; value?: string };
						if (!key) {
							res.statusCode = 400;
							res.end(JSON.stringify({ success: false }));
							return;
						}
						// arena-2fa: the dup-TOTP toggle is read by the rust server
						// (UserAuthService.validateOtp parity) — forward it before
						// acking so the test's set/reset ordering holds. Misc test
						// env vars have no effect on the rust server; ack only.
						if (key === 'MISSKEY_TEST_CHECK_DUPLICATED_TOTP') {
							try {
								await fetch(`http://127.0.0.1:${PORT}/api/_test/env`, {
									method: 'POST',
									headers: { 'content-type': 'application/json' },
									body: JSON.stringify({ key, value }),
								});
							} catch (e) {
								console.error('[mkrs-harness] env forward failed', e);
							}
						}
						// arena-oauth: the /oauth/authorize loopback-guard toggle is
						// read by the rust server (OAuth2ProviderService
						// #resolveAuthorizationRequest parity) — same forward lane.
						if (key === 'MISSKEY_TEST_CHECK_IP_RANGE') {
							try {
								await fetch(`http://127.0.0.1:${PORT}/api/_test/env`, {
									method: 'POST',
									headers: { 'content-type': 'application/json' },
									body: JSON.stringify({ key, value }),
								});
							} catch (e) {
								console.error('[mkrs-harness] env forward failed', e);
							}
						}
						res.end(JSON.stringify({ success: true }));
					} catch {
						res.statusCode = 400;
						res.end(JSON.stringify({ success: false }));
					}
				})();
			});
			return;
		}
		res.statusCode = 404;
		res.end();
	});
	await new Promise<void>((resolve) => {
		controller!.listen(PORT + 1000, 'localhost', () => resolve());
	});

	console.log('[mkrs-harness] application initialized.');
}

export async function teardown(): Promise<void> {
	child?.kill('SIGTERM');
	child = undefined;
	await new Promise<void>((resolve) => controller?.close(() => resolve()));
	controller = undefined;
}
