import assert from 'node:assert/strict';
import test from 'node:test';

function installWindow(server) {
    const previousWindow = globalThis.window;
    globalThis.window = server ? { __TAURITAVERN_SERVER__: true } : {};
    return () => {
        if (previousWindow === undefined) delete globalThis.window;
        else globalThis.window = previousWindow;
    };
}

async function importFreshRoutes() {
    return import(`../src/tauri/main/routes/index.js?t=${Date.now()}-${Math.random()}`);
}

test('server-native routes bypass the in-page router in server mode', async () => {
    const restore = installWindow(true);
    try {
        const { shouldHandleInPage } = await importFreshRoutes();
        for (const path of [
            '/csrf-token',
            '/api/bootstrap',
            '/api/chats/get',
            '/api/chats/save',
            '/api/backends/chat-completions/generate',
            '/api/backends/chat-completions/status',
        ]) {
            assert.equal(shouldHandleInPage(path), false, path);
        }
    } finally {
        restore();
    }
});

test('the app still answers server-native route names in-page', async () => {
    const restore = installWindow(false);
    try {
        const { shouldHandleInPage } = await importFreshRoutes();
        assert.equal(shouldHandleInPage('/api/bootstrap'), true);
        assert.equal(shouldHandleInPage('/api/chats/save'), true);
        assert.equal(shouldHandleInPage('/api/backends/chat-completions/generate'), true);
    } finally {
        restore();
    }
});

test('server mode keeps non-native upstream routes in-page', async () => {
    const restore = installWindow(true);
    try {
        const { shouldHandleInPage } = await importFreshRoutes();
        assert.equal(shouldHandleInPage('/api/settings/get'), true);
        assert.equal(shouldHandleInPage('/api/characters/all'), true);
        assert.equal(shouldHandleInPage('/api/worldinfo/get'), true);
    } finally {
        restore();
    }
});
