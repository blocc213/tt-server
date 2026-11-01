import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { webcrypto } from 'node:crypto';

test('HTTP host preserves raw options, loaded-byte CAS, resource ownership and conflict status', async t => {
    const previous = { window: globalThis.window, fetch: globalThis.fetch, crypto: globalThis.crypto, identity: globalThis.__TAURITAVERN_HOST__ };
    t.after(() => {
        globalThis.window = previous.window;
        globalThis.fetch = previous.fetch;
        Object.defineProperty(globalThis, 'crypto', { configurable: true, value: previous.crypto });
        globalThis.__TAURITAVERN_HOST__ = previous.identity;
    });
    globalThis.window = {};
    globalThis.__TAURITAVERN_HOST__ = { platform: 'browser', kind: 'server' };
    Object.defineProperty(globalThis, 'crypto', { configurable: true, value: webcrypto });
    const source = await readFile(new URL('../src/tauri-bridge.js', import.meta.url), 'utf8');
    const bridge = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}#${Math.random()}`);
    const target = { kind: 'character', characterId: 'alice', fileName: 'story' };
    const version = { byte_len: 100, sha256: 'a'.repeat(64) };
    const nextVersion = { byte_len: 120, sha256: 'b'.repeat(64) };
    const requests = [];
    const responses = [
        Response.json({ sourceId: 1, readerId: 2 }, { headers: { 'x-tauritavern-chat-version': JSON.stringify(version) } }),
        Response.json({ sessionId: 's1', maxFrameBytes: 4096 }),
        Response.json(4),
        Response.json({ version: nextVersion }),
        Response.json({ sessionId: 's2', maxFrameBytes: 4096 }),
        new Response(new Uint8Array([1, 2]), { headers: { 'Content-Type': 'application/octet-stream' } }),
        Response.json(null),
        Response.json({ error: 'integrity' }, { status: 409 }),
    ];
    globalThis.fetch = async (url, options) => { requests.push({ url, options }); return responses.shift(); };
    await bridge.invoke('open_cold_chat', { target });
    await bridge.invoke('begin_chat_commit', { target, operation: { kind: 'metadata' } });
    const bytes = new Uint8Array([1, 2, 3, 4]);
    await bridge.invoke('append_chat_commit_chunk', bytes, { headers: { 'session-id': 's1', offset: '0' } });
    await bridge.invoke('finish_chat_commit', { sessionId: 's1', expectedSize: 4 });
    await bridge.invoke('begin_chat_commit', { target, operation: { kind: 'metadataExtension', namespace: 'plugin' } });
    assert.deepEqual(new Uint8Array(await bridge.invoke('read_bytes', { rid: 2 })), new Uint8Array([1, 2]));
    await bridge.invoke('plugin:resources|close', { rid: 1 });
    await assert.rejects(bridge.invoke('finish_chat_commit', { sessionId: 's2', expectedSize: 0 }), error => error.code === 'integrity' && error.status === 409);
    assert.deepEqual(JSON.parse(requests[1].options.body).version, version);
    assert.equal(JSON.parse(requests[1].options.body).isNew, false);
    assert.deepEqual(JSON.parse(requests[4].options.body).version, nextVersion);
    assert.equal(requests[2].url, '/rpc-raw/append_chat_commit_chunk');
    assert.equal(requests[2].options.body, bytes);
    assert.equal(requests[2].options.headers.get('session-id'), 's1');
    assert.equal(requests[6].url, '/rpc/close_resource');
    const clients = new Set(requests.map(request => request.options.headers.get('x-tauritavern-client-id')));
    assert.equal(clients.size, 1);
    assert.ok([...clients][0]);
    assert.equal(bridge.isTauri(), false);
    await assert.rejects(bridge.invoke('register_agent_extension_tool', {}), /not available on server host/);
});

test('browser save policy preserves page and offers full unsaved export', async () => {
    const source = await readFile(new URL('../src/script.js', import.meta.url), 'utf8');
    const policy = source.slice(source.indexOf('async function confirmChatIntegrityOverwrite'), source.indexOf('/** The `chat_metadata`'));
    assert.match(policy, /Keep this page/);
    assert.match(policy, /Export a copy/);
    assert.match(policy, /hydrateMessageSwipes/);
    assert.match(policy, /popup\.result === 1002\) window\.location\.reload/);
    assert.match(policy, /throw error/);
    assert.match(policy, /timeOut: 0/);
});

test('new server chat uses MustNotExist and pagehide closes page resources', async t => {
    const oldWindow = globalThis.window;
    const oldFetch = globalThis.fetch;
    const oldIdentity = globalThis.__TAURITAVERN_HOST__;
    const oldCrypto = globalThis.crypto;
    t.after(() => {
        globalThis.window = oldWindow;
        globalThis.fetch = oldFetch;
        globalThis.__TAURITAVERN_HOST__ = oldIdentity;
        Object.defineProperty(globalThis, 'crypto', { configurable: true, value: oldCrypto });
    });
    globalThis.window = new EventTarget();
    globalThis.__TAURITAVERN_HOST__ = { platform: 'browser', kind: 'server' };
    Object.defineProperty(globalThis, 'crypto', { configurable: true, value: webcrypto });
    const requests = [];
    globalThis.fetch = async (url, options) => {
        requests.push({ url, options, body: JSON.parse(options.body) });
        return Response.json(url.includes('open_') ? 42 : { sessionId: 'new', maxFrameBytes: 4096 });
    };
    const source = await readFile(new URL('../src/tauri-bridge.js', import.meta.url), 'utf8');
    const bridge = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}#${Math.random()}`);
    await bridge.invoke('begin_chat_commit', { target: { kind: 'group', chatId: 'new' }, operation: { kind: 'payload', force: false } });
    assert.equal(requests[0].body.isNew, true);
    assert.equal(requests[0].body.version, null);
    await bridge.invoke('open_chat_backup_download', { name: 'backup' });
    window.dispatchEvent(new Event('pagehide'));
    assert.equal(requests.at(-1).url, '/rpc/close_resource');
    assert.equal(requests.at(-1).options.keepalive, true);
    assert.equal(requests.at(-1).body.rid, 42);
});
