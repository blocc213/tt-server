import test from 'node:test';
import assert from 'node:assert/strict';

import {
    createLifecycleFlushService,
    TAURI_GRACEFUL_EXIT_EVENT,
} from '../src/tauri/main/services/lifecycle/lifecycle-flush-service.js';

class FakeEventTarget {
    constructor() {
        this.listeners = new Map();
    }

    addEventListener(type, listener) {
        const listeners = this.listeners.get(type) ?? new Set();
        listeners.add(listener);
        this.listeners.set(type, listeners);
    }

    removeEventListener(type, listener) {
        this.listeners.get(type)?.delete(listener);
    }

    dispatch(type) {
        for (const listener of this.listeners.get(type) ?? []) {
            listener({ type });
        }
    }

    listenerCount(type) {
        return this.listeners.get(type)?.size ?? 0;
    }
}

test('host lifecycle listeners are installed once and fan out to registered flushers', async () => {
    const windowObject = new FakeEventTarget();
    const documentObject = new FakeEventTarget();
    documentObject.visibilityState = 'visible';
    const calls = [];
    const service = createLifecycleFlushService({
        windowObject, documentObject, listen: async () => () => {}, logger: { error() {} },
    });

    service.register('session', reason => calls.push(`session:${reason}`));
    service.register('invokes', reason => calls.push(`invokes:${reason}`));
    service.install();
    service.install();

    assert.equal(windowObject.listenerCount('pagehide'), 1);
    assert.equal(windowObject.listenerCount('beforeunload'), 1);
    assert.equal(documentObject.listenerCount('visibilitychange'), 1);

    windowObject.dispatch('pagehide');
    await service.waitForIdle();
    assert.deepEqual(calls, ['session:pagehide', 'invokes:pagehide']);

    documentObject.visibilityState = 'hidden';
    documentObject.dispatch('visibilitychange');
    await service.waitForIdle();
    assert.deepEqual(calls.slice(-2), ['session:visibilitychange:hidden', 'invokes:visibilitychange:hidden']);
});
test('one failing lifecycle flusher does not block the remaining handlers', async () => {
    const windowObject = new FakeEventTarget();
    const documentObject = new FakeEventTarget();
    const errors = [];
    const calls = [];
    const service = createLifecycleFlushService({
        windowObject,
        documentObject,
        logger: { error: (...args) => errors.push(args) },
    });

    service.register('broken', () => {
        throw new Error('broken');
    });
    service.register('healthy', reason => calls.push(reason));

    await assert.rejects(service.flush('test'), /broken/);

    assert.deepEqual(calls, ['test']);
    assert.equal(errors.length, 1);
});

test('native exit waits for a successful flush before destroying the window', async () => {
    const windowObject = new FakeEventTarget();
    const documentObject = new FakeEventTarget();
    let nativeExitHandler;
    let destroyed = false;
    let releaseFlush;
    const flushBlock = new Promise(resolve => {
        releaseFlush = resolve;
    });

    const service = createLifecycleFlushService({
        windowObject, documentObject, logger: { error() {} },
        listen: async (event, handler) => {
            assert.equal(event, TAURI_GRACEFUL_EXIT_EVENT);
            nativeExitHandler = handler;
            return () => {};
        },
        getCurrentWindow: () => ({ destroy: async () => { destroyed = true; } }),
    });
    service.register('settings', () => flushBlock);
    service.install();
    await Promise.resolve();

    const exitTask = nativeExitHandler();
    await Promise.resolve();
    assert.equal(destroyed, false);

    releaseFlush();
    await exitTask;
    assert.equal(destroyed, true);
});

test('native exit keeps the window open when persistence fails', async () => {
    const windowObject = new FakeEventTarget();
    const documentObject = new FakeEventTarget();
    const errors = [];
    let nativeExitHandler;
    let destroyed = false;

    const service = createLifecycleFlushService({
        windowObject,
        documentObject,
        logger: { error: (...args) => errors.push(args) },
        listen: async (_event, handler) => {
            nativeExitHandler = handler;
            return () => {};
        },
        getCurrentWindow: () => ({ destroy: async () => { destroyed = true; } }),
    });
    service.register('settings', () => Promise.reject(new Error('disk full')));
    service.install();
    await Promise.resolve();

    await nativeExitHandler();

    assert.equal(destroyed, false);
    assert.match(String(errors.at(-1)?.[0]), /keeping the app open/);
});

test('concurrent flush calls reuse the in-flight promise and run handlers once', async () => {
    const service = createLifecycleFlushService({
        windowObject: new FakeEventTarget(),
        documentObject: new FakeEventTarget(),
        logger: { error() {} },
    });
    const calls = [];
    let resolveHandler;
    const handlerBlock = new Promise(resolve => {
        resolveHandler = resolve;
    });

    service.register('session', async reason => {
        calls.push(reason);
        await handlerBlock;
    });

    const firstFlush = service.flush('first');
    const concurrentFlush = service.flush('second');
    assert.strictEqual(firstFlush, concurrentFlush);

    await Promise.resolve();
    assert.deepEqual(calls, ['first']);

    resolveHandler();
    await firstFlush;

    const nextFlush = service.flush('third');
    assert.notStrictEqual(firstFlush, nextFlush);
    await nextFlush;
    assert.deepEqual(calls, ['first', 'third']);
});

test('real server context starts without native APIs and retains browser lifecycle flushing', async t => {
    const previousWindow = globalThis.window;
    const previousDocument = globalThis.document;
    const previousIdentity = Object.getOwnPropertyDescriptor(globalThis, '__TAURITAVERN_HOST__');
    t.after(() => {
        globalThis.window = previousWindow;
        globalThis.document = previousDocument;
        if (previousIdentity) Object.defineProperty(globalThis, '__TAURITAVERN_HOST__', previousIdentity);
        else delete globalThis.__TAURITAVERN_HOST__;
    });
    globalThis.window = new FakeEventTarget();
    globalThis.document = new FakeEventTarget();
    globalThis.document.visibilityState = 'visible';
    Object.defineProperty(globalThis, '__TAURITAVERN_HOST__', { configurable: true, value: { platform: 'browser', kind: 'server' } });
    const { createTauriMainContext } = await import('../src/tauri/main/context/index.js');
    const context = createTauriMainContext({ invoke: async () => null });
    assert.equal(typeof context.invokeTransport, 'function');
    assert.equal(window.listenerCount('pagehide'), 1);
    assert.equal(window.listenerCount('beforeunload'), 1);
    assert.equal(document.listenerCount('visibilitychange'), 1);
    const { registerLifecycleFlushHandler, flushLifecycleState } = await import('../src/tauri/main/services/lifecycle/lifecycle-flush-service.js');
    const reasons = [];
    const unregister = registerLifecycleFlushHandler('server-boot-test', reason => reasons.push(reason));
    try {
        window.dispatch('pagehide');
        await flushLifecycleState('wait-for-pagehide');
        assert.deepEqual(reasons, ['pagehide']);
    } finally {
        unregister();
    }
});
