import assert from 'node:assert/strict';
import test from 'node:test';
import { registerExtensionRoutes } from '../src/tauri/main/routes/extensions-routes.js';
import { createRouteRegistry } from '../src/tauri/main/router.js';
import { jsonResponse } from '../src/tauri/main/http-utils.js';

test('browser migration import stages its archive, passes no arbitrary native path, and cleans up', async t => {
    const previous = globalThis.window;
    globalThis.window = { __TAURITAVERN_SERVER__: true };
    t.after(() => { globalThis.window = previous; });
    let cleaned = false;
    const router = createRouteRegistry();
    registerExtensionRoutes(router, {
        materializeUploadFile: async (blob, options) => {
            assert.equal(await blob.text(), 'archive');
            assert.equal(options.kind, 'data-archive');
            return { filePath: '/staging/archive.zip', isTemporary: true, cleanup: async () => { cleaned = true; } };
        },
        safeInvoke: async (command, args) => {
            assert.equal(command, 'start_import_data_archive');
            assert.deepEqual(args, { archivePath: '/staging/archive.zip' });
            return 'job-1';
        },
    }, { jsonResponse });
    const body = new FormData();
    body.set('archive', new Blob(['archive']), 'archive.zip');
    const response = await router.handle({ method: 'POST', path: '/api/extensions/data-migration/import', body });
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { ok: true, job_id: 'job-1' });
    assert.equal(cleaned, true);
});
