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
        return {
            ok: next.status >= 200 && next.status < 300,
            status: next.status,
            async text() {
                return JSON.stringify(next.body);
            },
        };
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

test('server chat saves advance the revision returned by each successful save', async () => {
    const version1 = { byte_len: 100, sha256: 'a'.repeat(64) };
    const version2 = { byte_len: 110, sha256: 'b'.repeat(64) };
    const version3 = { byte_len: 120, sha256: 'c'.repeat(64) };
    const runtime = installServerRuntime([
        { status: 200, body: { chat: [header, message('ten')], version: version1 } },
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
        { status: 200, body: { chat: [header, message('ten')], version } },
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
