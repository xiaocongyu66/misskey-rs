// vitest config for running upstream e2e specs against misskey-rs.
// Same shape as packages/backend/vitest.config.e2e.ts, but:
//  - globalSetup spawns the rust server (e2e-harness/global-setup.mts)
//  - include is narrowed via E2E_INCLUDE (first gate: users.ts)
import { EventEmitter } from 'node:events';
import { resolve } from 'node:path';
import { defineConfig } from 'vitest/config';

EventEmitter.defaultMaxListeners = 20;

const backendDir = import.meta.dirname;

export default defineConfig({
	test: {
		dir: backendDir,
		include: [process.env.E2E_INCLUDE ?? 'test/e2e/users.ts'],
		globalSetup: resolve(backendDir, './global-setup.mts'),
		setupFiles: ['./test/setup.e2e.ts'],
		testTimeout: 60_000,
		maxWorkers: 1,
		restoreMocks: true,
	},
	resolve: {
		alias: {
			'@': resolve(backendDir, 'src'),
		},
	},
});
