import test from 'node:test';
import assert from 'node:assert/strict';
import { initializeShareTargetImport } from '../src/scripts/share-target-import.js';

test('browser without native share bridge starts without affecting ordinary import handlers', t => {
    const previous = globalThis.window;
    t.after(() => { globalThis.window = previous; });
    globalThis.window = {};
    assert.equal(initializeShareTargetImport({
        importFromExternalUrl: () => assert.fail('no native URL payload exists'),
        processDroppedFiles: () => assert.fail('no native file payload exists'),
    }), undefined);
});

test('native host still subscribes and imports shared URLs in order', async t => {
    const previous = globalThis.window;
    t.after(() => { globalThis.window = previous; });
    let receive;
    const unsubscribe = () => {};
    const imported = [];
    let complete;
    const finished = new Promise(resolve => { complete = resolve; });
    globalThis.window = { __TAURITAVERN_NATIVE_SHARE__: {
        subscribe(handler) { receive = handler; return unsubscribe; },
    } };
    assert.equal(initializeShareTargetImport({
        async importFromExternalUrl(url) { imported.push(url); if (imported.length === 2) complete(); },
        processDroppedFiles: () => assert.fail('URL sharing must not import files'),
    }), unsubscribe);
    receive({ kind: 'url', url: 'https://example.com/first' });
    receive({ kind: 'url', url: 'https://example.com/second' });
    await finished;
    assert.deepEqual(imported, ['https://example.com/first', 'https://example.com/second']);
});
