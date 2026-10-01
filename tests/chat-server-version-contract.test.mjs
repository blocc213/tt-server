import assert from 'node:assert/strict';
import test from 'node:test';

function installServerRuntime(responses) {
    const previousWindow = globalThis.window;
    const previousFetch = globalThis.fetch;
    const requests = [];

    globalThis.window = { __TAURITAVERN_SERVER__: true };
    globalThis.fetch = async (url, init = {}) => {
        const body = init.body ? JSON.parse(init.body) : null;
        requests.push({ url: String(url), body });
        const next = responses.shift();
        assert.ok(next, `unexpected request ${url}`);
        return new Response(JSON.stringify(next.body), {
            status: next.status,
            headers: next.headers ?? {},
        });
    };

    return {
        requests,
        restore() {
            if (previousWindow === undefined) delete globalThis.window;
            else globalThis.window = previousWindow;
            globalThis.fetch = previousFetch;
        },
    };
}

async function importFreshTransport() {
    return import(`../src/scripts/tauri/chat/transport.js?t=${Date.now()}-${Math.random()}`);
}

const header = {
    chat_metadata: { integrity: 'shared-slug' },
    user_name: 'User',
    character_name: 'Alice',
};

const message = text => ({
    name: 'Alice', is_user: false, send_date: '2026-01-01', mes: text, extra: {},
});

// Upstream's bare-array body; the revision rides in a response header.
const chatRead = (chat, version) => ({
    status: 200,
    body: chat,
    headers: { 'x-tauritavern-chat-version': JSON.stringify(version) },
});

test('server chat reads return the upstream bare array extensions index into', async () => {
    const runtime = installServerRuntime([chatRead([header, message('ten')], { byte_len: 1, sha256: 'a'.repeat(64) })]);

    try {
        const transport = await importFreshTransport();
        const chat = await transport.loadCharacterChatPayload({
            characterName: 'Alice', avatarUrl: 'alice.png', fileName: 'story',
        });
        assert.deepEqual(chat, [header, message('ten')]);
    } finally {
        runtime.restore();
    }
});

test('server chat saves advance the revision returned by each successful save', async () => {
    const version1 = { byte_len: 100, sha256: 'a'.repeat(64) };
    const version2 = { byte_len: 110, sha256: 'b'.repeat(64) };
    const version3 = { byte_len: 120, sha256: 'c'.repeat(64) };
    const runtime = installServerRuntime([
        chatRead([header, message('ten')], version1),
        { status: 200, body: { ok: true, version: version2 } },
        { status: 200, body: { ok: true, version: version3 } },
    ]);

    try {
        const transport = await importFreshTransport();
        await transport.loadCharacterChatPayload({
            characterName: 'Alice', avatarUrl: 'alice.png', fileName: 'story',
        });
        await transport.saveCharacterChatPayload({
            characterName: 'Alice', avatarUrl: 'alice.png', fileName: 'story',
            payload: [header, message('twenty')],
        });
        await transport.saveCharacterChatPayload({
            characterName: 'Alice', avatarUrl: 'alice.png', fileName: 'story',
            payload: [header, message('thirty')],
        });

        assert.deepEqual(runtime.requests[1].body.version, version1);
        assert.equal(runtime.requests[1].body.is_new, false);
        assert.deepEqual(runtime.requests[2].body.version, version2);
        assert.equal(runtime.requests[2].body.is_new, false);
    } finally {
        runtime.restore();
    }
});

test('a new server chat is explicitly marked MustNotExist on first save', async () => {
    const runtime = installServerRuntime([
        { status: 200, body: { ok: true, version: { byte_len: 1, sha256: 'a'.repeat(64) } } },
    ]);

    try {
        const transport = await importFreshTransport();
        await transport.saveCharacterChatPayload({
            characterName: 'Alice', avatarUrl: 'alice.png', fileName: 'new-story',
            payload: [header, message('one')],
        });

        assert.equal(runtime.requests[0].body.version, null);
        assert.equal(runtime.requests[0].body.is_new, true);
    } finally {
        runtime.restore();
    }
});

test('server integrity rejection preserves the code used by the conflict UI', async () => {
    const version = { byte_len: 100, sha256: 'a'.repeat(64) };
    const runtime = installServerRuntime([
        chatRead([header, message('ten')], version),
        { status: 400, body: { error: 'integrity' } },
    ]);

    try {
        const transport = await importFreshTransport();
        await transport.loadCharacterChatPayload({
            characterName: 'Alice', avatarUrl: 'alice.png', fileName: 'story',
        });

        await assert.rejects(
            transport.saveCharacterChatPayload({
                characterName: 'Alice', avatarUrl: 'alice.png', fileName: 'story',
                payload: [header, message('stale')],
            }),
            error => error?.code === 'integrity',
        );
    } finally {
        runtime.restore();
    }
});

// Upstream ST extensions (e.g. LittleWhiteBox) save without TT's revision fields.
const legacySave = (fileName = 'story', extra = {}) => ({
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ ch_name: 'Alice', avatar_url: 'alice.png', file_name: fileName, chat: [header, message('map')], force: false, ...extra }),
});
const active = () => ({ characterId: 'alice', fileName: 'story' });

async function adapt(transport, init, send, activeTarget = active) {
    return transport.adaptLegacyServerChatSave({
        url: new URL('http://tt.local/api/chats/save'), input: '/api/chats/save', init, send, activeTarget,
    });
}

test('a legacy save of the open chat carries the loaded revision and advances it', async () => {
    const version1 = { byte_len: 100, sha256: 'a'.repeat(64) };
    const version2 = { byte_len: 110, sha256: 'b'.repeat(64) };
    const runtime = installServerRuntime([
        chatRead([header, message('ten')], version1),
        { status: 200, body: { ok: true, version: version2 } },
    ]);

    try {
        const transport = await importFreshTransport();
        await transport.loadCharacterChatPayload({ characterName: 'Alice', avatarUrl: 'alice.png', fileName: 'story' });

        const response = await adapt(transport, legacySave(), globalThis.fetch);
        assert.equal(response.status, 200);
        assert.deepEqual(await response.json(), { ok: true, version: version2 }, 'caller still reads the body');
        assert.deepEqual(runtime.requests[1].body.version, version1);
        assert.equal(runtime.requests[1].body.is_new, false);
        assert.deepEqual(runtime.requests[1].body.chat, [header, message('map')]);
        assert.deepEqual(transport.getLoadedChatVersion('alice', 'story'), version2);
    } finally {
        runtime.restore();
    }
});

test('a rejected legacy save keeps the status and does not advance the revision', async () => {
    const version1 = { byte_len: 100, sha256: 'a'.repeat(64) };
    const runtime = installServerRuntime([
        chatRead([header, message('ten')], version1),
        { status: 400, body: { error: 'integrity' } },
    ]);

    try {
        const transport = await importFreshTransport();
        await transport.loadCharacterChatPayload({ characterName: 'Alice', avatarUrl: 'alice.png', fileName: 'story' });

        const response = await adapt(transport, legacySave(), globalThis.fetch);
        assert.equal(response.status, 400);
        assert.deepEqual(transport.getLoadedChatVersion('alice', 'story'), version1);
    } finally {
        runtime.restore();
    }
});

test('legacy saves are left alone unless they target the open, loaded chat without TT fields', async () => {
    const version1 = { byte_len: 100, sha256: 'a'.repeat(64) };
    const runtime = installServerRuntime([chatRead([header, message('ten')], version1)]);
    const send = () => assert.fail('must not send');

    try {
        const transport = await importFreshTransport();
        await transport.loadCharacterChatPayload({ characterName: 'Alice', avatarUrl: 'alice.png', fileName: 'story' });

        assert.equal(await adapt(transport, legacySave('other'), send), null, 'other chat of the same character');
        assert.equal(await adapt(transport, legacySave(), send, () => ({ characterId: 'bob', fileName: 'story' })), null, 'user switched chats');
        assert.equal(await adapt(transport, legacySave(), send, () => null), null, 'group or no active chat');
        assert.equal(await adapt(transport, legacySave('story', { version: version1, is_new: false }), send), null, 'TT native save');
        assert.equal(await adapt(transport, legacySave('story', { force: true }), send), null, 'explicit overwrite');
    } finally {
        runtime.restore();
    }
});

test('a legacy save with no loaded revision is not upgraded to a create or overwrite', async () => {
    const runtime = installServerRuntime([]);
    try {
        const transport = await importFreshTransport();
        assert.equal(await adapt(transport, legacySave(), () => assert.fail('must not send')), null);
    } finally {
        runtime.restore();
    }
});
