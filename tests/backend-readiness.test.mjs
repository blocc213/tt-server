import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const READINESS_PATH = path.join(REPO_ROOT, 'src/tauri/main/bootstrap/backend-readiness.js');

async function importFreshReadiness() {
    const url = `${pathToFileURL(READINESS_PATH).href}?t=${Date.now()}-${Math.random()}`;
    return import(url);
}

function installFakeWindow({ invoke } = {}) {
    const calls = [];

    global.window = {
        __TAURI__: {
            core: {
                async invoke(command) {
                    calls.push(command);
                    return invoke?.(command);
                },
            },
        },
    };

    return { calls };
}

function installServerWindow({ status = 200, body = 'null' } = {}) {
    const calls = [];

    global.window = { __TAURITAVERN_SERVER__: true };
    global.fetch = async (url, init) => {
        calls.push({ url: String(url), body: init?.body });
        return {
            ok: status >= 200 && status < 300,
            status,
            async text() {
                return body;
            },
        };
    };

    return { calls };
}

function cleanupGlobals() {
    delete global.window;
    delete global.fetch;
}

test('backend readiness follows default content initialization', async () => {
    const source = await readFile(
        path.join(REPO_ROOT, 'src-tauri/crates/tauritavern/src/app.rs'),
        'utf8',
    );
    const contentIndex = source.indexOf('.initialize_default_content("default-user")');
    const stateIndex = source.indexOf('app_handle.manage(state.clone())');
    const readyIndex = source.indexOf('backend_readiness.mark_ready()');

    assert.ok(contentIndex >= 0 && contentIndex < stateIndex && stateIndex < readyIndex);
});

test('backend readiness waits on the explicit readiness command', async () => {
    const fake = installFakeWindow({
        invoke(command) {
            assert.equal(command, 'wait_for_backend_ready');
        },
    });

    try {
        const { waitForBackendReady } = await importFreshReadiness();
        await waitForBackendReady();

        assert.deepEqual(fake.calls, ['wait_for_backend_ready']);
    } finally {
        cleanupGlobals();
    }
});

test('backend readiness fails fast when no backend transport is reachable', async () => {
    // Neither the app's injected IPC nor server mode: waiting must reject rather
    // than hang, so startup surfaces the failure instead of stalling on a
    // blank screen.
    global.window = {};

    try {
        const { waitForBackendReady } = await importFreshReadiness();

        await assert.rejects(waitForBackendReady());
    } finally {
        cleanupGlobals();
    }
});

test('backend readiness uses the server transport when served over HTTP', async () => {
    const fake = installServerWindow();

    try {
        const { waitForBackendReady } = await importFreshReadiness();
        await waitForBackendReady();

        assert.equal(fake.calls.length, 1);
        assert.match(fake.calls[0].url, /\/rpc\/wait_for_backend_ready$/);
    } finally {
        cleanupGlobals();
    }
});

test('backend readiness propagates a server-side startup failure', async () => {
    installServerWindow({ status: 500, body: JSON.stringify({ error: 'backend failed' }) });

    try {
        const { waitForBackendReady } = await importFreshReadiness();

        await assert.rejects(waitForBackendReady(), /backend failed/);
    } finally {
        cleanupGlobals();
    }
});

test('backend readiness propagates backend startup failure', async () => {
    installFakeWindow({
        invoke() {
            throw new Error('backend failed');
        },
    });

    try {
        const { waitForBackendReady } = await importFreshReadiness();

        await assert.rejects(waitForBackendReady(), /backend failed/);
    } finally {
        cleanupGlobals();
    }
});
