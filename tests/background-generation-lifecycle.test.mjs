import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

async function setup(hidden = false) {
    const events = new EventTarget();
    globalThis.window = events;
    globalThis.document = Object.assign(new EventTarget(), { visibilityState: hidden ? 'hidden' : 'visible' });
    Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { onLine: true } });
    const values = {};
    globalThis.localStorage = new Proxy(values, { get(target, key) {
        if (key === 'getItem') return name => target[name] ?? null;
        if (key === 'setItem') return (name, value) => { target[name] = value; };
        if (key === 'removeItem') return name => { delete target[name]; };
        return target[key];
    } });
    const calls = [];
    globalThis.fetch = async (url, options) => {
        calls.push({ url, body: JSON.parse(options.body) });
        return new Response('data: [DONE]\n\n', { headers: { 'Content-Type': 'text/event-stream' } });
    };
    const original = await readFile(new URL('../src/scripts/tauritavern/background-generation.js', import.meta.url), 'utf8');
    // Isolate the real module from the application bootstrap and its DOM imports.
    const source = original.replace(/^import .*;\n/gm, '')
        + '\nconst isServerEnv = () => true; const accountStorage = {getItem:()=>"true"}; const uuidv4 = () => `test-${++nextId}`; let nextId = 0;';
    const module = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}#${Math.random()}`);
    return { module, calls, events, values };
}

for (const lifecycle of ['pagehide', 'beforeunload']) {
    test(`${lifecycle} detaches without cancelling`, async () => {
        const { module, calls, events } = await setup();
        const controller = new AbortController();
        await module.sendBackgroundGenerateRequest({}, true, controller.signal, () => ({}));
        events.dispatchEvent(new Event(lifecycle));
        controller.abort();
        assert.deepEqual(calls.map(call => call.url), ['/api/backends/chat-completions/generate']);
        assert.equal(Object.keys(localStorage).length, 1);
    });
}

test('explicit Stop cancels exactly once', async () => {
    const { module, calls } = await setup();
    const controller = new AbortController();
    await module.sendBackgroundGenerateRequest({}, true, controller.signal, () => ({}));
    controller.abort();
    controller.abort();
    assert.equal(calls.filter(call => call.url.endsWith('/cancel')).length, 1);
    assert.equal(Object.keys(localStorage).length, 0);
});

test('hidden page immediately posts the first generate request', async () => {
    const { module, calls } = await setup(true);
    await module.sendBackgroundGenerateRequest({}, true, new AbortController().signal, () => ({}));
    assert.equal(calls[0].url, '/api/backends/chat-completions/generate');
});

test('reopened job applies once; expired job never generates', async () => {
    const { module, calls } = await setup();
    const context = { chatId: 'chat', characterId: 'avatar', groupId: null };
    const record = { ...context, streamId: 'old', stream: true, createdAt: Date.now() };
    localStorage.setItem('tt_bg_job:old', JSON.stringify(record));
    let applied = 0;
    const recover = () => module.recoverBackgroundJobs(context, async response => response.text(), async text => {
        assert.equal(text, 'data: [DONE]\n\n'); applied++;
    }, () => {}, () => ({}));
    await recover();
    await recover();
    assert.equal(applied, 1);
    localStorage.setItem('tt_bg_job:old', JSON.stringify(record));
    let notification;
    globalThis.fetch = async (url, options) => { calls.push({ url, body: JSON.parse(options.body) }); return new Response('', { status: 404 }); };
    await module.recoverBackgroundJobs(context, () => assert.fail('expired content consumed'), () => assert.fail('expired content applied'), kind => { notification = kind; }, () => ({}));
    assert.equal(notification, 'expired');
    assert.equal(Object.keys(localStorage).length, 0);
    assert.ok(calls.every(call => call.url.endsWith('/resume')));
});

test('stream target belongs only to the explicitly identified request', async () => {
    const { module, values } = await setup();
    const context = { type: 'normal', messageCount: 3 };
    const first = await module.sendBackgroundGenerateRequest({}, true, new AbortController().signal, () => ({}), context);
    const second = await module.sendBackgroundGenerateRequest({}, true, new AbortController().signal, () => ({}), context);
    assert.equal(typeof first.backgroundJobId, 'string');
    assert.notEqual(first.backgroundJobId, second.backgroundJobId);
    const message = { mes: '' };
    module.markBackgroundTarget(message, first.backgroundJobId);
    assert.equal(message.extra?.background_job_id, first.backgroundJobId);
    const records = Object.values(values).map(value => JSON.parse(value));
    assert.equal(records.find(record => record.streamId === first.backgroundJobId)?.targetToken, first.backgroundJobId);
    assert.equal(records.find(record => record.streamId === second.backgroundJobId)?.targetToken, undefined);
    const foregroundMessage = { mes: 'foreground' };
    module.markBackgroundTarget(foregroundMessage, undefined);
    assert.equal(foregroundMessage.extra, undefined);
});

test('OpenAI transport requires request opt-in and enabled setting', async () => {
    const { module, calls, values } = await setup();
    const source = await readFile(new URL('../src/scripts/openai.js', import.meta.url), 'utf8');
    // Execute the real request function without loading the DOM-heavy application bootstrap.
    const requestSource = source.slice(source.indexOf('async function sendOpenAIRequest('), source.indexOf('\nfunction parseChatCompletionStream('));
    let enabled = true;
    const dependencies = {
        getChatCompletionModel: () => 'test-model', oai_settings: {},
        createGenerationParameters: async () => ({ generate_data: {}, stream: true, canMultiSwipe: false }),
        eventSource: { emit: async () => {} }, event_types: {},
        isBackgroundGenerationEnabled: () => enabled,
        sendBackgroundGenerateRequest: module.sendBackgroundGenerateRequest,
        getRequestHeaders: () => ({}), backgroundContext: type => ({ type }),
        usesClaudeMessagesSemantics: () => false,
        getChatCompletionRequestContext: () => ({ model: 'test-model' }),
        parseChatCompletionStream: response => Object.assign(async function* () {}, { backgroundJobId: response.backgroundJobId }),
    };
    const send = new Function(...Object.keys(dependencies), `${requestSource}; return sendOpenAIRequest;`)(...Object.values(dependencies));
    await send('normal', []);
    await send('normal', [], undefined, { background: false });
    await send('quiet', []);
    assert.ok(calls.every(call => call.body.background === undefined));
    assert.equal(Object.keys(values).length, 0);
    const generator = await send('normal', [], undefined, { background: true });
    assert.equal(calls.at(-1).body.background, true);
    assert.equal(calls.at(-1).body.stream_id, generator.backgroundJobId);
    assert.ok(values[`tt_bg_job:${generator.backgroundJobId}`]);
    enabled = false;
    await send('normal', [], undefined, { background: true });
    assert.equal(calls.at(-1).body.background, undefined);
});

test('recovery emits generation end once only after a successful chat save', async () => {
    const { module } = await setup();
    const source = await readFile(new URL('../src/scripts/openai.js', import.meta.url), 'utf8');
    const recoverySource = source.slice(source.indexOf('async function recoverCurrentBackgroundJobs('), source.indexOf('\n/**', source.indexOf('async function recoverCurrentBackgroundJobs(')));
    const chat = [{ mes: 'user', is_user: true }];
    const context = { chatId: 'chat', characterId: 'avatar', groupId: null };
    const order = [];
    let saveFails = false;
    const dependencies = {
        chat, isBackgroundGenerationAvailable: () => true,
        backgroundContext: type => ({ ...context, type, messageCount: chat.length, target: JSON.stringify(chat.at(-1)) }),
        recoverBackgroundJobs: module.recoverBackgroundJobs,
        extractMessageFromData: data => data.choices[0].message.content,
        saveReply: async reply => { order.push('reply'); chat.push({ mes: reply.getMessage, is_user: false }); },
        saveChatConditional: async () => { if (saveFails) throw new Error('save failed'); order.push('saved'); },
        eventSource: { emit: async (event, index) => { order.push([event, index]); } },
        event_types: { GENERATION_ENDED: 'generation_ended' }, getRequestHeaders: () => ({}),
        toastr: { info: () => {}, error: () => {}, warning: () => {} }, t: strings => strings[0],
    };
    window.prompt = () => {};
    globalThis.fetch = async () => Response.json({ choices: [{ message: { content: 'recovered' } }] });
    const recover = new Function(...Object.keys(dependencies), `${recoverySource}; return recoverCurrentBackgroundJobs;`)(...Object.values(dependencies));
    const enqueue = (streamId, type = 'normal') => localStorage.setItem(`tt_bg_job:${streamId}`, JSON.stringify({
        ...context, streamId, type, stream: false, createdAt: Date.now(),
        messageCount: chat.length, target: JSON.stringify(chat.at(-1)),
    }));
    enqueue('success');
    await recover();
    await recover();
    assert.deepEqual(order, ['reply', 'saved', ['generation_ended', 2]]);
    order.length = 0;
    enqueue('unsupported', 'impersonate');
    await recover();
    assert.deepEqual(order, []);
    enqueue('expired');
    globalThis.fetch = async () => new Response('', { status: 404 });
    await recover();
    assert.deepEqual(order, []);
    enqueue('failed-save');
    globalThis.fetch = async () => Response.json({ choices: [{ message: { content: 'not-saved' } }] });
    saveFails = true;
    await recover();
    assert.deepEqual(order, ['reply']);
});
