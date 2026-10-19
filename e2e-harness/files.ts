/*
 * SPDX-FileCopyrightText: syuilo and misskey-project
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// arena-files: harness-side driver spec. Upstream (pinned ea2c3aed) has no
// test/e2e/files.ts — the drive.ts e2e (already green) only owns create /
// upload-from-url-callback / attached-notes. This spec drives the REST of the
// file endpoint surface at scale, with every assert traced to the pinned
// endpoint sources: packages/backend/src/server/api/endpoints/drive/files/*
// (list/show/update/delete/find/find-by-hash/check-existence/create) +
// DriveService updateFile/deleteFile legs + QueryService.makePaginationQuery.

process.env.NODE_ENV = 'test';

import * as assert from 'node:assert';
import { readFile } from 'node:fs/promises';
import { basename } from 'node:path';
import { describe, beforeAll, test } from 'vitest';
import { Blob, FormData } from 'node-fetch';
import { api, relativeFetch, signup, uploadFile } from '../utils.js';
import type * as misskey from 'misskey-js';

describe('Drive files', () => {
	let alice: misskey.entities.SignupResponse;
	let bob: misskey.entities.SignupResponse;
	let fileA: misskey.entities.DriveFile;
	let fileB: misskey.entities.DriveFile;
	let fileC: misskey.entities.DriveFile;
	let arenaFolder: misskey.entities.DriveFolder;

	beforeAll(async () => {
		alice = await signup({ username: 'alice' });
		bob = await signup({ username: 'bob' });
		// three identical root uploads: the pagination fixture (ids strictly
		// increase with every request — aid monotonicity)
		fileA = (await uploadFile(alice, { path: '192.jpg' })).body!;
		fileB = (await uploadFile(alice, { path: '192.jpg' })).body!;
		fileC = (await uploadFile(alice, { path: '192.jpg' })).body!;
	}, 1000 * 60 * 2);

	// utils.uploadFile only carries the file/force/name triangle — the
	// create.ts folderId/comment/isSensitive params ride the same multipart
	// contract, so this mirrors it verbatim (file part, force=true, fields).
	const formUpload = async (user: misskey.entities.SignupResponse, fields: Record<string, string> = {}): Promise<{
		status: number;
		body: misskey.entities.DriveFile | any;
	}> => {
		// upstream ships fixtures at test/resources/; test/utils.ts:316 resolves
		// 'resources/' relative to test/utils.ts (one level up from test/e2e/),
		// so mirror that from this spec's test/e2e/ location
		const absPath = new URL('192.jpg', new URL('../resources/', import.meta.url));
		const formData = new FormData();
		formData.append('file', new Blob([new Uint8Array(await readFile(absPath))]), basename(absPath.toString()));
		formData.append('force', 'true');
		for (const [k, v] of Object.entries(fields)) formData.append(k, v);
		if (user) formData.append('i', user.token);
		const res = await relativeFetch('api/drive/files/create', {
			method: 'POST',
			body: formData,
		});
		const body = res.status !== 204 ? await res.json() : null;
		return { status: res.status, body };
	};

	test('レナ', async () => {
		const named = await formUpload(alice, { name: 'レナ.jpg' });
		assert.strictEqual(named.status, 200);
		assert.strictEqual(named.body.name, 'レナ.jpg');
		assert.strictEqual(named.body.type, 'image/jpeg');
		assert.strictEqual(named.body.isSensitive, false);
		assert.strictEqual(named.body.folderId, null);

		// create.ts: isSensitive/comment form params persist
		const flagged = await formUpload(alice, { name: 'sens', isSensitive: 'true', comment: '顔' });
		assert.strictEqual(flagged.status, 200);
		assert.strictEqual(flagged.body.isSensitive, true);
		assert.strictEqual(flagged.body.comment, '顔');

		// create.ts: validateFileName failure → INVALID_FILE_NAME
		// (id f449b209-0c60-4e51-84d5-29486263bfd4)
		const bad = await formUpload(alice, { name: 'te/st' });
		assert.strictEqual(bad.status, 400);
		assert.strictEqual((bad.body as any).error.code, 'INVALID_FILE_NAME');
	});

	test('ファイル一覧を取得できる', async () => {
		// makePaginationQuery default: id DESC (newest first)
		let res = await api('drive/files', {}, alice);
		assert.strictEqual(res.status, 200);
		const ids = res.body.map((f: misskey.entities.DriveFile) => f.id);
		const cix = ids.indexOf(fileC.id);
		assert.notStrictEqual(cix, -1);
		assert.deepStrictEqual(ids.slice(cix, cix + 3), [fileC.id, fileB.id, fileA.id]);

		// untilId is exclusive (QueryService L34-37)
		res = await api('drive/files', { untilId: fileB.id }, alice);
		assert.deepStrictEqual(res.body.map((f: misskey.entities.DriveFile) => f.id), [fileA.id]);

		// sinceId is exclusive and flips the direction to ASC (oldest first)
		res = await api('drive/files', { sinceId: fileA.id }, alice);
		const asc = res.body.map((f: misskey.entities.DriveFile) => f.id);
		const bix = asc.indexOf(fileB.id);
		assert.deepStrictEqual(asc.slice(bix, bix + 2), [fileB.id, fileC.id]);

		// limit narrows the page (paramDef 1..100 default 10)
		res = await api('drive/files', { limit: 2 }, alice);
		assert.strictEqual(res.body.length, 2);
		assert.strictEqual(res.body[0].id, ids[0]);

		// limit 0 is outside 1..100 → 400
		res = await api('drive/files', { limit: 0 }, alice);
		assert.strictEqual(res.status, 400);
	});

	test('フォルダと型で絞り込める', async () => {
		arenaFolder = (await api('drive/folders/create', { name: 'arena' }, alice)).body;
		assert.strictEqual(arenaFolder.name, 'arena');

		const fFold = await formUpload(alice, { folderId: arenaFolder.id, name: 'fold' });
		assert.strictEqual(fFold.status, 200);

		// files.ts: `if (ps.folderId)` → folderId = :folderId … else IS NULL
		let res = await api('drive/files', { folderId: arenaFolder.id }, alice);
		assert.deepStrictEqual(res.body.map((f: misskey.entities.DriveFile) => f.id), [fFold.body.id]);
		res = await api('drive/files', { folderId: null }, alice);
		assert.ok(!res.body.some((f: misskey.entities.DriveFile) => f.id === fFold.body.id));

		// type filter: exact match, wildcard `image/*` (files.ts LIKE arm)
		res = await api('drive/files', { type: 'image/png' }, alice);
		assert.ok(!res.body.some((f: misskey.entities.DriveFile) => f.id === fileA.id));
		res = await api('drive/files', { type: 'image/jpeg', limit: 100 }, alice);
		assert.ok(res.body.some((f: misskey.entities.DriveFile) => f.id === fileA.id));
		res = await api('drive/files', { type: 'image/*', limit: 100 }, alice);
		assert.ok(res.body.some((f: misskey.entities.DriveFile) => f.id === fileA.id));
	});

	test('ファイルのプロパティを取得できる', async () => {
		// show.ts: pack(file, { detail: true, withUser: true, self: true })
		let res = await api('drive/files/show', { fileId: fileA.id }, alice);
		assert.strictEqual(res.status, 200);
		assert.strictEqual(res.body.id, fileA.id);
		assert.strictEqual(res.body.name, '192.jpg');
		assert.strictEqual(res.body.type, 'image/jpeg');
		assert.strictEqual(res.body.md5, fileA.md5);
		assert.strictEqual(res.body.url, fileA.url);
		assert.strictEqual(res.body.userId, alice.id);
		assert.strictEqual(res.body.user.id, alice.id);

		// show.ts anyOf: the url arm matches url / webpublicUrl / thumbnailUrl
		res = await api('drive/files/show', { url: fileA.url }, alice);
		assert.strictEqual(res.status, 200);
		assert.strictEqual(res.body.id, fileA.id);

		// unknown id → NO_SUCH_FILE (show.ts 067bc436-2718-4795-b0fb-ecbe43949e31)
		res = await api('drive/files/show', { fileId: 'z'.repeat(24) }, alice);
		assert.strictEqual(res.status, 400);
		assert.strictEqual((res.body as any).error.code, 'NO_SUCH_FILE');

		// show.ts: non-owner non-moderator → ACCESS_DENIED
		// (25b73c73-68b1-41d0-bad1-381cfdf6579f)
		res = await api('drive/files/show', { fileId: fileA.id }, bob);
		assert.strictEqual(res.status, 400);
		assert.strictEqual((res.body as any).error.code, 'ACCESS_DENIED');
	});

	test('ファイルのプロパティを更新できる', async () => {
		// DriveService.updateFile: name/comment/isSensitive persist and the
		// packed file echoes them
		let res = await api('drive/files/update', {
			fileId: fileC.id,
			name: '更新済.png',
			comment: 'だいめい',
			isSensitive: true,
		}, alice);
		assert.strictEqual(res.status, 200);
		assert.strictEqual(res.body.name, '更新済.png');
		assert.strictEqual(res.body.comment, 'だいめい');
		assert.strictEqual(res.body.isSensitive, true);
		assert.strictEqual(res.body.folderId, null);

		// update.ts: folderId re-parents; root lists lose the file
		res = await api('drive/files/update', { fileId: fileC.id, folderId: arenaFolder.id }, alice);
		assert.strictEqual(res.status, 200);
		assert.strictEqual(res.body.folderId, arenaFolder.id);
		const root = await api('drive/files', { folderId: null }, alice);
		assert.ok(!root.body.some((f: misskey.entities.DriveFile) => f.id === fileC.id));

		// update.ts → DriveService.InvalidFileNameError → INVALID_FILE_NAME
		res = await api('drive/files/update', { fileId: fileC.id, name: '..\\x' }, alice);
		assert.strictEqual(res.status, 400);
		assert.strictEqual((res.body as any).error.code, 'INVALID_FILE_NAME');

		// update.ts → DriveService.NoSuchFolderError → NO_SUCH_FOLDER
		res = await api('drive/files/update', { fileId: fileC.id, folderId: 'f'.repeat(24) }, alice);
		assert.strictEqual(res.status, 400);
		assert.strictEqual((res.body as any).error.code, 'NO_SUCH_FOLDER');

		// update.ts: non-owner non-moderator → ACCESS_DENIED
		res = await api('drive/files/update', { fileId: fileA.id, name: 'steal' }, bob);
		assert.strictEqual(res.status, 400);
		assert.strictEqual((res.body as any).error.code, 'ACCESS_DENIED');
	});

	test('ファイルを削除できる', async () => {
		const gone = (await uploadFile(alice, { path: '192.jpg' })).body!;

		// delete.ts → driveService.deleteFile → row gone, 204 void
		const res = await api('drive/files/delete', { fileId: gone.id }, alice);
		assert.strictEqual(res.status, 204);
		assert.strictEqual(res.body, null);

		const shown = await api('drive/files/show', { fileId: gone.id }, alice);
		assert.strictEqual(shown.status, 400);
		assert.strictEqual((shown.body as any).error.code, 'NO_SUCH_FILE');

		// deleting what is no longer there → NO_SUCH_FILE
		// (delete.ts 908939ec-e52b-4458-b395-1025195cea58)
		const again = await api('drive/files/delete', { fileId: gone.id }, alice);
		assert.strictEqual(again.status, 400);
		assert.strictEqual((again.body as any).error.code, 'NO_SUCH_FILE');

		// delete.ts: non-owner non-moderator → ACCESS_DENIED
		// (5eb8d909-2540-4970-90b8-dd6f86088121)
		const foreign = await api('drive/files/delete', { fileId: fileA.id }, bob);
		assert.strictEqual(foreign.status, 400);
		assert.strictEqual((foreign.body as any).error.code, 'ACCESS_DENIED');
	});

	test('名前とハッシュで検索できる', async () => {
		// find.ts: name + userId (+ folderId ?? IsNull) exact match
		const mine = await formUpload(alice, { name: 'findme' });
		assert.strictEqual(mine.status, 200);
		let res = await api('drive/files/find', { name: 'findme.jpg' }, alice);
		assert.ok(res.body.some((f: misskey.entities.DriveFile) => f.id === mine.body.id));

		// the renamed+foldered fileC (T5 moved it) is only found inside its folder
		res = await api('drive/files/find', { name: '更新済.png', folderId: arenaFolder.id }, alice);
		assert.deepStrictEqual(res.body.map((f: misskey.entities.DriveFile) => f.id), [fileC.id]);
		res = await api('drive/files/find', { name: '更新済.png' }, alice);
		assert.deepStrictEqual(res.body, []);

		// find-by-hash.ts: md5 + userId
		res = await api('drive/files/find-by-hash', { md5: fileA.md5 }, alice);
		assert.ok(res.body.some((f: misskey.entities.DriveFile) => f.id === fileA.id));
		res = await api('drive/files/find-by-hash', { md5: '0'.repeat(32) }, alice);
		assert.deepStrictEqual(res.body, []);

		// check-existence.ts: EXISTS(md5 + userId) — owner-scoped
		res = await api('drive/files/check-existence', { md5: fileA.md5 }, alice);
		assert.strictEqual(res.body, true);
		res = await api('drive/files/check-existence', { md5: fileA.md5 }, bob);
		assert.strictEqual(res.body, false);
		res = await api('drive/files/check-existence', {}, alice);
		assert.strictEqual(res.status, 400);
	});

	test('URLからのアップロードはバリデーションされる', async () => {
		// upload-from-url.ts paramDef: url required (ajv) → INVALID_PARAM
		let res = await api('drive/files/upload-from-url', { marker: 'm' } as any, alice);
		assert.strictEqual(res.status, 400);
		assert.strictEqual((res.body as any).error.code, 'INVALID_PARAM');

		// the handler is fire-and-forget: a garbage url still acks 204 and
		// the failed download (DriveService.uploadFromUrl → downloadUrl)
		// never lands a file
		const before = await api('drive/files', { limit: 100 }, alice);
		res = await api('drive/files/upload-from-url', { url: 'not a url at all' }, alice);
		assert.strictEqual(res.status, 204);
		await new Promise((resolve) => setTimeout(resolve, 1500));
		const after = await api('drive/files', { limit: 100 }, alice);
		assert.strictEqual(after.body.length, before.body.length);
	});
});
