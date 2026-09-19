import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

test('chat saves remain serialized across character and group entrypoints', async () => {
    const script = await readFile(path.join(REPO_ROOT, 'src/script.js'), 'utf8');
    const groups = await readFile(path.join(REPO_ROOT, 'src/scripts/group-chats.js'), 'utf8');

    assert.match(script, /export function enqueueChatSave\s*\(/);
    assert.match(script, /export async function saveChat[\s\S]*?return\s+enqueueChatSave\s*\(/);
    assert.match(script, /export async function saveChatConditional[\s\S]*?enqueueChatSave\s*\(/);
    assert.match(groups, /async function saveGroupChat[\s\S]*?return\s+enqueueChatSave\s*\(/);

    const saveChatUnsafe = script.slice(
        script.indexOf('async function saveChatUnsafe'),
        script.indexOf('\n/**', script.indexOf('async function saveChatUnsafe')),
    );
    const saveChatConditional = script.slice(
        script.indexOf('export async function saveChatConditional'),
        script.indexOf('\n/**', script.indexOf('export async function saveChatConditional')),
    );
    const saveGroupChatUnsafe = groups.slice(
        groups.indexOf('async function saveGroupChatUnsafe'),
        groups.indexOf('\n/**', groups.indexOf('async function saveGroupChatUnsafe')),
    );

    assert.match(saveChatUnsafe, /if \(!isIntegrityError\)[\s\S]*?toastr\.error[\s\S]*?throw error;/);
    assert.match(saveGroupChatUnsafe, /if \(!isIntegrityError\)[\s\S]*?toastr\.error[\s\S]*?throw error;/);
    assert.match(saveChatConditional, /await Promise\.all\(\[savePromise, postSavePromise\]\);/);
    assert.doesNotMatch(saveChatConditional, /catch\s*\(/);
});


test('character and group loads reject stale async results', async () => {
    const script = await readFile(path.join(REPO_ROOT, 'src/script.js'), 'utf8');
    const groups = await readFile(path.join(REPO_ROOT, 'src/scripts/group-chats.js'), 'utf8');

    const getChat = script.slice(
        script.indexOf('export async function getChat'),
        script.indexOf('\nasync function getChatResult'),
    );
    assert.match(getChat, /const\s+startedChid\s*=\s*this_chid/);
    assert.match(getChat, /const\s+stillActive\s*=[\s\S]*?if\s*\(!stillActive\)\s*\{\s*return;\s*\}/);

    const getGroupChat = groups.slice(
        groups.indexOf('export async function getGroupChat'),
        groups.indexOf('\n/**', groups.indexOf('export async function getGroupChat')),
    );
    assert.match(getGroupChat, /const\s+startedGroupId\s*=\s*groupId/);
    assert.match(getGroupChat, /const\s+isStillActive\s*=/);
    assert.match(getGroupChat, /if\s*\(!isStillActive\(\)\)\s*\{\s*return;\s*\}/);
});

test('missing chat payload only creates a greeting when explicitly allowed', async () => {
    const script = await readFile(path.join(REPO_ROOT, 'src/script.js'), 'utf8');
    const getChat = script.slice(
        script.indexOf('export async function getChat'),
        script.indexOf('\nasync function getChatResult'),
    );

    assert.match(getChat, /allowNewChat\s*=\s*false/);
    assert.match(getChat, /allow_not_found:\s*allowNewChat/);
    assert.doesNotMatch(getChat, /allowNotFound:\s*allowNewChat/);

    const getChatResult = script.slice(
        script.indexOf('async function getChatResult'),
        script.indexOf('\n/**', script.indexOf('async function getChatResult')),
    );
    assert.match(getChatResult, /if\s*\(\s*allowNewChat\s*&&\s*chat\.length\s*===\s*0\s*\)/);
});

test('group bookmark save preserves an explicit branch snapshot', async () => {
    const groups = await readFile(path.join(REPO_ROOT, 'src/scripts/group-chats.js'), 'utf8');
    const saveBookmark = groups.slice(
        groups.indexOf('export async function saveGroupBookmarkChat'),
        groups.indexOf('\nfunction onSendTextareaInput'),
    );

    assert.match(saveBookmark, /saveGroupBookmarkChat\(groupId,\s*name,\s*metadata,\s*mesId,\s*chatData\s*=\s*undefined\)/);
    assert.match(saveBookmark, /Array\.isArray\(chatData\)\s*\?\s*chatData/);
});

test('hide and unhide keep upstream range-only command semantics', async () => {
    const source = await readFile(path.join(REPO_ROOT, 'src/scripts/slash-commands.js'), 'utf8');
    const commands = source.slice(
        source.indexOf("name: 'hide'"),
        source.indexOf("name: 'member-get'"),
    );

    assert.match(commands, /typeList:\s*\[ARGUMENT_TYPE\.NUMBER,\s*ARGUMENT_TYPE\.RANGE\]/g);
    assert.match(commands, /enumProvider:\s*commonEnumProviders\.messages\(\)/g);
    assert.doesNotMatch(commands, /loaded window|unloaded|SlashCommandEnumValue\(['"](?:all|before)/);
});

test('slash swipe commands target the physical chat tail', async () => {
    const source = await readFile(path.join(REPO_ROOT, 'src/scripts/slash-commands.js'), 'utf8');

    assert.match(source, /async function addSwipeCallback[\s\S]*const lastMessage = chat\[chat\.length - 1\]/);
    assert.match(source, /async function deleteSwipeCallback[\s\S]*deleteSwipe\(swipeId, chat\.length - 1\)/);
});

test('inactive chat DOM keeps content-visibility containment', async () => {
    const style = await readFile(path.join(REPO_ROOT, 'src/style.css'), 'utf8');

    assert.match(style, /#chat\s*>\s*\.mes:not\(\.last_mes\)[\s\S]*?content-visibility:\s*auto/);
    assert.match(style, /contain-intrinsic-block-size:\s*auto 200px/);
});
