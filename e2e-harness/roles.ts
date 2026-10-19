/*
 * SPDX-FileCopyrightText: syuilo and misskey-project
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// roles.ts e2e — authored per the pinned upstream semantics (misskey-dev/misskey
// @ ea2c3aed): src/core/RoleService.ts (evalCond + getUserRoles/getUserPolicies),
// RoleEntityService.pack, server/api/endpoints/admin/roles/{create,assign,
// unassign,delete,list,show,update,users}.ts and the UserEntityService.pack
// `roles`/`badgeRoles` fields. Run against misskey-rs by
// .github/workflows/e2e-roles.yml (E2E_INCLUDE test/e2e/roles.ts).

process.env.NODE_ENV = 'test';

import * as assert from 'node:assert';
import { beforeAll, describe, test } from 'vitest';
import { api, failedApiCall, post, role, signup, successfulApiCall } from '../utils.js';
import type * as misskey from 'misskey-js';
import { DEFAULT_POLICIES } from '@/core/RoleService.js';

describe('ロール', () => {
	let root: misskey.entities.SignupResponse;
	let alice: misskey.entities.SignupResponse;
	let bob: misskey.entities.SignupResponse;
	let carol: misskey.entities.SignupResponse;
	let dave: misskey.entities.SignupResponse;
	let mod: misskey.entities.SignupResponse;
	let badgeUser: misskey.entities.SignupResponse;
	let silUser: misskey.entities.SignupResponse;

	let roleAdmin: misskey.entities.Role;
	let rolePublic: misskey.entities.Role;
	let roleModerator: misskey.entities.Role;
	let roleBadge: misskey.entities.Role;
	let roleSilenced: misskey.entities.Role;
	let roleEditable: misskey.entities.Role;

	// conditional roles (target: 'conditional' + condFormula — RoleService.evalCond)
	let crCat: misskey.entities.Role; // isCat (policies: canPublicNote false)
	let crBotNot: misskey.entities.Role; // not isCat
	let crLocalCat: misskey.entities.Role; // and [isLocal, isCat]
	let crYoung: misskey.entities.Role; // createdLessThan 10y
	let crOld: misskey.entities.Role; // createdMoreThan 1d
	let crNoFol: misskey.entities.Role; // followersLessThanOrEq 0
	let crHasFol: misskey.entities.Role; // followersMoreThanOrEq 1
	let crNotes: misskey.entities.Role; // notesMoreThanOrEq 1
	let crAssigned: misskey.entities.Role; // roleAssignedTo rolePublic

	const defaultPolicies = (): any => {
		return Object.fromEntries(Object.entries(DEFAULT_POLICIES).map(([k, v]) => [k, {
			priority: 0,
			useDefault: true,
			value: v,
		}]));
	};

	// UserEntityService.pack L563 — the public-roles projection
	const roleLite = (role: misskey.entities.Role) => ({
		id: role.id,
		name: role.name,
		color: role.color,
		iconUrl: role.iconUrl,
		description: role.description,
		isModerator: role.isModerator,
		isAdministrator: role.isAdministrator,
		displayOrder: role.displayOrder,
	});

	const show = async (userId: string, me = root): Promise<misskey.entities.UserDetailed> => {
		return successfulApiCall({ endpoint: 'users/show', parameters: { userId }, user: me });
	};

	beforeAll(async () => {
		root = await signup({ username: 'root' });
		alice = await signup({ username: 'alice' });
		bob = await signup({ username: 'bob' });
		carol = await signup({ username: 'carol' });
		dave = await signup({ username: 'dave' });
		mod = await signup({ username: 'mod' });
		badgeUser = await signup({ username: 'badgeuser' });
		silUser = await signup({ username: 'siluser' });

		// manual roles
		roleAdmin = await role(root, { isAdministrator: true, name: 'Admin Role', displayOrder: 1 });
		rolePublic = await role(root, { isPublic: true, name: 'Public Role', displayOrder: 10 });
		roleModerator = await role(root, { isModerator: true, name: 'Moderator Role', displayOrder: 2 });
		roleBadge = await role(root, { asBadge: true, isPublic: true, name: 'Badge Role', displayOrder: 4, iconUrl: 'https://example.com/badge.png' });
		roleSilenced = await role(root, { name: 'Silenced Role', displayOrder: 3 }, { canPublicNote: { priority: 0, useDefault: false, value: false } });
		roleEditable = await role(root, { canEditMembersByModerator: true, name: 'Editable Role', displayOrder: 5 });

		// conditional roles — condFormula shapes from src/models/Role.ts
		// cat: public + badge + silencing policy
		crCat = await successfulApiCall({
			endpoint: 'admin/roles/create',
			parameters: {
				name: 'Cat Role', target: 'conditional', isPublic: true, asBadge: true,
				displayOrder: 30, iconUrl: 'https://example.com/cat.png',
				condFormula: { id: '0cc0f0e2-e20e-4b04-9db2-4a5b5ec20d19', type: 'isCat' } as any,
				policies: {
					...defaultPolicies(),
					canPublicNote: { priority: 0, useDefault: false, value: false },
				},
			},
			user: root,
		}) as misskey.entities.Role;
		crBotNot = await successfulApiCall({
			endpoint: 'admin/roles/create',
			parameters: {
				name: 'Not Cat Role', target: 'conditional', isPublic: true, displayOrder: 31,
				condFormula: { id: '0cc0f0e3-e20e-4b04-9db2-4a5b5ec20d19', type: 'not', value: { id: '0cc0f0e4-e20e-4b04-9db2-4a5b5ec20d19', type: 'isCat' } } as any,
				policies: defaultPolicies(),
			},
			user: root,
		}) as misskey.entities.Role;
		crLocalCat = await successfulApiCall({
			endpoint: 'admin/roles/create',
			parameters: {
				name: 'Local Cat Role', target: 'conditional', isPublic: true, displayOrder: 32,
				condFormula: { id: '0cc0f0e5-e20e-4b04-9db2-4a5b5ec20d19', type: 'and', values: [
					{ id: '0cc0f0e6-e20e-4b04-9db2-4a5b5ec20d19', type: 'isLocal' },
					{ id: '0cc0f0e7-e20e-4b04-9db2-4a5b5ec20d19', type: 'isCat' },
				] } as any,
				policies: defaultPolicies(),
			},
			user: root,
		}) as misskey.entities.Role;
		crYoung = await successfulApiCall({
			endpoint: 'admin/roles/create',
			parameters: {
				name: 'Young Role', target: 'conditional', isPublic: true, displayOrder: 33,
				condFormula: { id: '0cc0f0e8-e20e-4b04-9db2-4a5b5ec20d19', type: 'createdLessThan', sec: 86400 * 3650 } as any,
				policies: defaultPolicies(),
			},
			user: root,
		}) as misskey.entities.Role;
		crOld = await successfulApiCall({
			endpoint: 'admin/roles/create',
			parameters: {
				name: 'Old Role', target: 'conditional', isPublic: true, displayOrder: 34,
				condFormula: { id: '0cc0f0e9-e20e-4b04-9db2-4a5b5ec20d19', type: 'createdMoreThan', sec: 86400 } as any,
				policies: defaultPolicies(),
			},
			user: root,
		}) as misskey.entities.Role;
		crNoFol = await successfulApiCall({
			endpoint: 'admin/roles/create',
			parameters: {
				name: 'No Follower Role', target: 'conditional', isPublic: true, displayOrder: 35,
				condFormula: { id: '0cc0f0ea-e20e-4b04-9db2-4a5b5ec20d19', type: 'followersLessThanOrEq', value: 0 } as any,
				policies: defaultPolicies(),
			},
			user: root,
		}) as misskey.entities.Role;
		crHasFol = await successfulApiCall({
			endpoint: 'admin/roles/create',
			parameters: {
				name: 'Followed Role', target: 'conditional', isPublic: true, displayOrder: 36,
				condFormula: { id: '0cc0f0eb-e20e-4b04-9db2-4a5b5ec20d19', type: 'followersMoreThanOrEq', value: 1 } as any,
				policies: defaultPolicies(),
			},
			user: root,
		}) as misskey.entities.Role;
		crNotes = await successfulApiCall({
			endpoint: 'admin/roles/create',
			parameters: {
				name: 'Note Taker Role', target: 'conditional', isPublic: true, displayOrder: 37,
				condFormula: { id: '0cc0f0ec-e20e-4b04-9db2-4a5b5ec20d19', type: 'notesMoreThanOrEq', value: 1 } as any,
				policies: defaultPolicies(),
			},
			user: root,
		}) as misskey.entities.Role;
		crAssigned = await successfulApiCall({
			endpoint: 'admin/roles/create',
			parameters: {
				name: 'Assigned To Role', target: 'conditional', isPublic: true, displayOrder: 38,
				condFormula: { id: '0cc0f0ed-e20e-4b04-9db2-4a5b5ec20d19', type: 'roleAssignedTo', roleId: rolePublic.id } as any,
				policies: defaultPolicies(),
			},
			user: root,
		}) as misskey.entities.Role;

		// manual assignments (rolePublic→alice happens inside the assign test)
		await api('admin/roles/assign', { userId: mod.id, roleId: roleModerator.id }, root);
		await api('admin/roles/assign', { userId: badgeUser.id, roleId: roleBadge.id }, root);
		await api('admin/roles/assign', { userId: silUser.id, roleId: roleSilenced.id }, root);

		// user-flag fixtures (the condFormula matrix): bob is local cat bot,
		// carol posts a note, dave follows carol (so carol has a follower,
		// dave has none)
		await api('i/update', { isCat: true, isBot: true }, bob);
		await post(carol, { text: 'note for notesCount' });
		await api('following/create', { userId: carol.id }, dave);
	}, 1000 * 60 * 10);

	test('を作成することができる（そのままRole packが返る）', async () => {
		const response = await successfulApiCall({ endpoint: 'admin/roles/show', parameters: { roleId: rolePublic.id }, user: root });
		assert.strictEqual(response.id, rolePublic.id);
		assert.strictEqual(response.name, 'Public Role');
		assert.strictEqual(response.description, '');
		assert.strictEqual(response.color, null);
		assert.strictEqual(response.target, 'manual');
		assert.strictEqual(response.isPublic, true);
		assert.strictEqual(response.isAdministrator, false);
		assert.strictEqual(response.isModerator, false);
		assert.strictEqual(response.isExplorable, false);
		assert.strictEqual(response.asBadge, false);
		assert.strictEqual(response.preserveAssignmentOnMoveAccount, false);
		assert.strictEqual(response.canEditMembersByModerator, false);
		assert.strictEqual(response.displayOrder, 10);
		// RoleEntityService.pack: createdAt from the AID, updatedAt ISO
		assert.strictEqual(response.createdAt, new Date(response.createdAt).toISOString());
		assert.strictEqual(response.updatedAt, new Date(response.updatedAt).toISOString());
		// policies echo + missing-key DEFAULT fill; no assignments yet
		assert.deepStrictEqual(response.policies, defaultPolicies());
		assert.strictEqual(response.usersCount, 0);
	});

	test('を作成することができる（空policiesでもDEFAULT_POLICIESが補完される）', async () => {
		const response = await successfulApiCall({
			endpoint: 'admin/roles/create',
			parameters: { name: 'Bare Role', policies: {} },
			user: root,
		}) as any;
		assert.deepStrictEqual(response.policies, defaultPolicies());
		assert.strictEqual(response.usersCount, 0);
		await successfulApiCall({ endpoint: 'admin/roles/delete', parameters: { roleId: response.id }, user: root });
	});

	test('を作成するとき権限が必要（モデレータロールがないとROLE_PERMISSION_DENIED）', async () => {
		await failedApiCall({ endpoint: 'admin/roles/create', parameters: { name: 'Nope' }, user: alice },
			{ status: 403, code: 'ROLE_PERMISSION_DENIED', id: 'd33d5333-db36-423d-a8f9-1a2b9549da41' });
		await failedApiCall({ endpoint: 'admin/roles/create', parameters: { name: 'Nope' }, user: undefined },
			{ status: 401, code: 'CREDENTIAL_REQUIRED', id: '1384574d-a912-4b81-8601-c7b1c4085df1' });
	});

	test('割り当てができる（usersCountとadmin/roles/usersに反映）', async () => {
		await successfulApiCall({ endpoint: 'admin/roles/assign', parameters: { userId: alice.id, roleId: rolePublic.id }, user: root });
		const response = await successfulApiCall({ endpoint: 'admin/roles/users', parameters: { roleId: rolePublic.id }, user: root });
		assert.strictEqual(response.length, 1);
		assert.strictEqual(response[0].user.id, alice.id);
		assert.strictEqual(response[0].user.username, 'alice');
		assert.strictEqual(response[0].expiresAt, null);
		assert.strictEqual(new Date(response[0].createdAt).toISOString(), response[0].createdAt);
		// duplicate live assignment → private AlreadyAssignedError → 500
		const dup = await api('admin/roles/assign', { userId: alice.id, roleId: rolePublic.id } as any, root as any);
		assert.strictEqual(dup.status, 500);
		// unknown role / user
		await failedApiCall({ endpoint: 'admin/roles/assign', parameters: { roleId: '000000000000', userId: alice.id } as any, user: root as any },
			{ status: 400, code: 'NO_SUCH_ROLE', id: '6503c040-6af4-4ed9-bf07-f2dd16678eab' });
		await failedApiCall({ endpoint: 'admin/roles/assign', parameters: { roleId: rolePublic.id, userId: '000000000000' } as any, user: root as any },
			{ status: 400, code: 'NO_SUCH_USER', id: '558ea170-f653-4700-94d0-5a818371d0df' });
		// plain user cannot assign at all (requireModerator)
		await failedApiCall({ endpoint: 'admin/roles/assign', parameters: { userId: bob.id, roleId: rolePublic.id } as any, user: carol as any },
			{ status: 403, code: 'ROLE_PERMISSION_DENIED', id: 'd33d5333-db36-423d-a8f9-1a2b9549da41' });
		// moderator passes requireModerator but the role is not canEditMembersByModerator
		await failedApiCall({ endpoint: 'admin/roles/assign', parameters: { userId: bob.id, roleId: rolePublic.id } as any, user: mod as any },
			{ status: 403, code: 'ACCESS_DENIED', id: '25b5bc31-dc79-4ebd-9bd2-c84978fd052c' });
		// ... but CAN edit members of an opt-in role
		await successfulApiCall({ endpoint: 'admin/roles/assign', parameters: { userId: badgeUser.id, roleId: roleEditable.id } as any, user: mod as any });
		const users = await successfulApiCall({ endpoint: 'admin/roles/users', parameters: { roleId: roleEditable.id }, user: root });
		assert.strictEqual(users.length, 1);
		assert.strictEqual(users[0].user.id, badgeUser.id);
		// uncredentialed assign
		await failedApiCall({ endpoint: 'admin/roles/assign', parameters: { userId: bob.id, roleId: rolePublic.id } as any, user: undefined },
			{ status: 401, code: 'CREDENTIAL_REQUIRED', id: '1384574d-a912-4b81-8601-c7b1c4085df1' });
	});

	test('割り当ては有効期限を尊重する（過去日付は黙って捨て、未来日付はusersCountに入る）', async () => {
		// already-expired requested expiry: assign.ts returns silently — no row
		await successfulApiCall({ endpoint: 'admin/roles/assign', parameters: { userId: carol.id, roleId: rolePublic.id, expiresAt: Date.now() - 1000 } as any, user: root as any });
		const usersPast = await successfulApiCall({ endpoint: 'admin/roles/users', parameters: { roleId: rolePublic.id }, user: root });
		assert.strictEqual(usersPast.length, 1);
		let rolePack = await successfulApiCall({ endpoint: 'admin/roles/show', parameters: { roleId: rolePublic.id }, user: root });
		assert.strictEqual(rolePack.usersCount, 1);
		// future expiry: row inserted, expiresAt echoed on admin/roles/users
		await successfulApiCall({ endpoint: 'admin/roles/assign', parameters: { userId: bob.id, roleId: roleEditable.id, expiresAt: Date.now() + 3600 * 1000 } as any, user: root as any });
		const usersFuture = await successfulApiCall({ endpoint: 'admin/roles/users', parameters: { roleId: roleEditable.id }, user: root });
		assert.strictEqual(usersFuture.length, 2);
		const fut = usersFuture.find(u => u.user.id === bob.id)!;
		assert.notStrictEqual(fut.expiresAt, null);
		assert.strictEqual(new Date(fut.expiresAt!).toISOString(), fut.expiresAt);
		rolePack = await successfulApiCall({ endpoint: 'admin/roles/show', parameters: { roleId: roleEditable.id }, user: root });
		assert.strictEqual(rolePack.usersCount, 2);
	});

	test('一覧を取得することができる（usersCount付き・モデレータゲート）', async () => {
		const response = await successfulApiCall({ endpoint: 'admin/roles/list', parameters: {}, user: root });
		const ids = response.map(r => r.id);
		assert.ok(ids.includes(rolePublic.id));
		assert.ok(ids.includes(crCat.id));
		assert.ok(!ids.includes('000000000000'));
		const pub = response.find(r => r.id === rolePublic.id)!;
		assert.strictEqual(pub.usersCount, 1);
		// moderator-gate: mod (has isModerator role) sees the list too
		const modList = await successfulApiCall({ endpoint: 'admin/roles/list', parameters: {}, user: mod as any });
		assert.ok(modList.some(r => r.id === rolePublic.id));
		await failedApiCall({ endpoint: 'admin/roles/list', parameters: {} as any, user: alice as any },
			{ status: 403, code: 'ROLE_PERMISSION_DENIED', id: 'd33d5333-db36-423d-a8f9-1a2b9549da41' });
	});

	test('をIDで取得することができる（NO_SUCH_ROLE）', async () => {
		const response = await successfulApiCall({ endpoint: 'admin/roles/show', parameters: { roleId: crCat.id }, user: mod as any });
		assert.strictEqual(response.id, crCat.id);
		assert.strictEqual(response.target, 'conditional');
		// conditional matches never count as assignment rows
		assert.strictEqual(response.usersCount, 0);
		await failedApiCall({ endpoint: 'admin/roles/show', parameters: { roleId: '000000000000' } as any, user: root as any },
			{ status: 400, code: 'NO_SUCH_ROLE', id: '07dc7d34-c0d8-49b7-96c6-db3ce64ee0b3' });
		await failedApiCall({ endpoint: 'admin/roles/users', parameters: { roleId: '000000000000' } as any, user: root as any },
			{ status: 400, code: 'NO_SUCH_ROLE', id: '224eff5e-2488-4b18-b3e7-f50d94421648' });
		await failedApiCall({ endpoint: 'admin/roles/users', parameters: { roleId: rolePublic.id } as any, user: alice as any },
			{ status: 403, code: 'ROLE_PERMISSION_DENIED', id: 'd33d5333-db36-423d-a8f9-1a2b9549da41' });
	});

	test('users/show にPublicロールがセットされていること（手動アサイン＋roleAssignedTo）', async () => {
		const response = await show(alice.id, bob);
		// alice = manual rolePublic + conditional crAssigned/crYoung/crNoFol/crBotNot
		// (fresh local users carry crYoung/crNoFol; roleAssignedTo matches the
		// manual assignment); displayOrder DESC, 8-key projection each
		assert.deepStrictEqual(response.badgeRoles, []);
		assert.deepStrictEqual(response.roles, [
			roleLite(crAssigned),
			roleLite(crNoFol),
			roleLite(crYoung),
			roleLite(crBotNot),
			roleLite(rolePublic),
		]);
	});

	test('users/show にPublicロールがセットされていること（catユーザー・バッヂ併用）', async () => {
		const response = await show(bob.id, alice);
		// bob is cat+bot: crCat + crLocalCat + crYoung + crNoFol; no manual roles;
		// crBotNot must NOT match (not isCat), crAssigned neither
		assert.deepStrictEqual(response.badgeRoles, [{
			name: crCat.name,
			iconUrl: crCat.iconUrl,
			displayOrder: crCat.displayOrder,
		}]);
		assert.deepStrictEqual(response.roles, [
			roleLite(crNoFol),
			roleLite(crYoung),
			roleLite(crLocalCat),
			roleLite(crCat),
		]);
		// conditional role policies merge: crCat forces canPublicNote false
		assert.strictEqual(response.isSilenced, true);
	});

	test('users/show にPublicロールがセットされていること（ノート・フォロワー条件）', async () => {
		const carolShown = await show(carol.id, alice);
		// carol: notesCount >= 1 → crNotes; dave follows her → crHasFol, NOT crNoFol
		assert.deepStrictEqual(carolShown.roles, [
			roleLite(crNotes),
			roleLite(crHasFol),
			roleLite(crYoung),
			roleLite(crBotNot),
		]);
		const daveShown = await show(dave.id, alice);
		// dave: 0 followers → crNoFol; 0 notes → no crNotes; not followed → no crHasFol
		assert.deepStrictEqual(daveShown.roles, [
			roleLite(crNoFol),
			roleLite(crYoung),
			roleLite(crBotNot),
		]);
		// fresh accounts are never crOld (createdMoreThan 1d)
		assert.ok(!carolShown.roles.some(r => r.id === crOld.id));
		assert.ok(!daveShown.roles.some(r => r.id === crOld.id));
	});

	test('users/show の一般ユーザーからはMod専用ロールが見えないこと', async () => {
		const response = await show(mod.id, bob);
		// roleModerator is not public: roles (public filter) holds only the conditionals
		assert.deepStrictEqual(response.roles, [
			roleLite(crNoFol),
			roleLite(crYoung),
			roleLite(crBotNot),
		]);
		// manual badge role rides the same user/show pack (UserLite badgeRoles)
		const badge = await show(badgeUser.id, bob);
		assert.deepStrictEqual(badge.roles, [
			roleLite(crNoFol),
			roleLite(crYoung),
			roleLite(crBotNot),
			roleLite(roleBadge),
		]);
		// silenced user: manual roleSilenced carries canPublicNote false → isSilenced
		const sil = await show(silUser.id, bob);
		assert.strictEqual(sil.isSilenced, true);
	});

	test('を更新することができる（update → show反映・権限・NO_SUCH_ROLE）', async () => {
		await successfulApiCall({ endpoint: 'admin/roles/update', parameters: { roleId: rolePublic.id, name: 'Renamed Public', displayOrder: 9, color: '#ff0000' } as any, user: root as any });
		rolePublic.name = 'Renamed Public';
		rolePublic.displayOrder = 9;
		rolePublic.color = '#ff0000';
		const response = await successfulApiCall({ endpoint: 'admin/roles/show', parameters: { roleId: rolePublic.id }, user: root });
		assert.strictEqual(response.name, 'Renamed Public');
		assert.strictEqual(response.displayOrder, 9);
		assert.strictEqual(response.color, '#ff0000');
		// moderator (not administrator) cannot update
		await failedApiCall({ endpoint: 'admin/roles/update', parameters: { roleId: rolePublic.id, name: 'X' } as any, user: mod as any },
			{ status: 403, code: 'ROLE_PERMISSION_DENIED', id: 'c3d38592-54c0-429d-be96-5636b0431a61' });
		await failedApiCall({ endpoint: 'admin/roles/update', parameters: { roleId: '000000000000', name: 'X' } as any, user: root as any },
			{ status: 400, code: 'NO_SUCH_ROLE', id: 'cd23ef55-09ad-428a-ac61-95a45e124b32' });
	});

	test('解除ができる（users/showから消え・二重解除は500）', async () => {
		await successfulApiCall({ endpoint: 'admin/roles/unassign', parameters: { userId: alice.id, roleId: rolePublic.id } as any, user: root as any });
		const response = await show(alice.id, bob);
		// the manual leg drops with its roleAssignedTo consequence
		assert.ok(!response.roles.some(r => r.id === rolePublic.id));
		assert.ok(!response.roles.some(r => r.id === crAssigned.id));
		// second unassign → RoleService.NotAssignedError is private → 500
		const dup = await api('admin/roles/unassign', { userId: alice.id, roleId: rolePublic.id } as any, root as any);
		assert.strictEqual(dup.status, 500);
		// unknown role/user errors (unassign.ts carries its own ids)
		await failedApiCall({ endpoint: 'admin/roles/unassign', parameters: { roleId: '000000000000', userId: alice.id } as any, user: root as any },
			{ status: 400, code: 'NO_SUCH_ROLE', id: '6e519036-a70d-4c76-b679-bc8fb18194e2' });
		await failedApiCall({ endpoint: 'admin/roles/unassign', parameters: { roleId: rolePublic.id, userId: '000000000000' } as any, user: root as any },
			{ status: 400, code: 'NO_SUCH_USER', id: '2b730f78-1179-461b-88ad-d24c9af1a5ce' });
	});

	test('を削除することができる（pack一式から消える）', async () => {
		await successfulApiCall({ endpoint: 'admin/roles/delete', parameters: { roleId: roleEditable.id } as any, user: root as any });
		await failedApiCall({ endpoint: 'admin/roles/show', parameters: { roleId: roleEditable.id } as any, user: root as any },
			{ status: 400, code: 'NO_SUCH_ROLE', id: '07dc7d34-c0d8-49b7-96c6-db3ce64ee0b3' });
		// assign on a deleted role hits NO_SUCH_ROLE first
		await failedApiCall({ endpoint: 'admin/roles/assign', parameters: { roleId: roleEditable.id, userId: bob.id } as any, user: root as any },
			{ status: 400, code: 'NO_SUCH_ROLE', id: '6503c040-6af4-4ed9-bf07-f2dd16678eab' });
		const listAfter = await successfulApiCall({ endpoint: 'admin/roles/list', parameters: {}, user: root });
		assert.ok(!listAfter.some(r => r.id === roleEditable.id));
	});
});
