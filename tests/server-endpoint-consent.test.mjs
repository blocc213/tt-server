import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

test('endpoint authorization requires explicit trust and retries only once', async () => {
    const source = (await readFile(new URL('../src/tauri/main/bootstrap/server-endpoint-consent.js', import.meta.url), 'utf8'))
        .replace(/^import .*;\n/, '').replace('export function', 'function');
    const grants = [];
    const install = new Function('invoke', `${source}; return installServerEndpointConsent;`)(async (...args) => grants.push(args));
    const endpoint = 'http://localhost:8080/v1';
    for (const allowed of [false, true]) {
        let calls = 0;
        const target = {
            location: new URL('http://tavern.local/'),
            confirm(warning) { assert.match(warning, /API keys, prompts and chat/); assert.match(warning, /HTTP is unencrypted/); return allowed; },
            async fetch() { calls++; return Response.json({ error: 'endpoint_authorization_required', endpoint }, { status: 403 }); },
        };
        install(target);
        const response = await target.fetch('/rpc/search_searxng', { method: 'POST', body: '{}' });
        assert.equal(response.status, 403);
        assert.equal(calls, allowed ? 2 : 1);
    }
    assert.deepEqual(grants, [['grant_user_endpoint', { endpoint, confirmed: true }]]);
});
