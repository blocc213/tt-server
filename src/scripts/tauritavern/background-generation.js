// Background generation for the browser (server) host.
//
// Phones suspend hidden pages and drop their sockets. When the user opts in,
// the server keeps the provider call running and buffers its output; this
// module re-attaches from the last event seen once the page can run again.
// Only Stop cancels. See docs/MobileBackgroundGenerationPlan.md.

import { isServerEnv } from '../../tauri-bridge.js';
import { accountStorage } from '../util/AccountStorage.js';
import { uuidv4 } from '../utils.js';

const STORAGE_KEY = 'tt_background_generation';
const GENERATE_URL = '/api/backends/chat-completions/generate';
const RESUME_URL = '/api/backends/chat-completions/resume';
const MAX_RESUME_ATTEMPTS = 20;

const JOB_PREFIX = 'tt_bg_job:';
const activeJobs = new Set();
export function markBackgroundTarget(message, streamId) {
    if (!activeJobs.has(streamId)) return;
    const key = JOB_PREFIX + streamId;
    const record = JSON.parse(localStorage.getItem(key) ?? 'null');
    if (!record) return;
    message.extra ??= {};
    message.extra.background_job_id = streamId;
    record.targetToken = streamId;
    record.targetAfterStart = JSON.stringify(message);
    record.messageCountAfterStart = record.messageCount + (record.type === 'normal' ? 1 : 0);
    localStorage.setItem(key, JSON.stringify(record));
}
let pageUnloading = false;
export function detachBackgroundPage() { pageUnloading = true; }
// Installed during module evaluation, before script.js installs its jQuery listener.
window.addEventListener('beforeunload', () => { pageUnloading = true; }, true);
window.addEventListener('pagehide', () => { pageUnloading = true; }, true);
window.addEventListener('pageshow', () => { pageUnloading = false; }, true);

export async function recoverBackgroundJobs(context, consume, apply, notify, getHeaders) {
    const keys = Object.keys(localStorage).filter(key => key.startsWith(JOB_PREFIX));
    for (const key of keys) {
        let record;
        try { record = JSON.parse(localStorage.getItem(key)); } catch { localStorage.removeItem(key); continue; }
        if (!record || Date.now() - record.createdAt > 2 * 60 * 60 * 1000) {
            localStorage.removeItem(key);
            continue;
        }
        if (activeJobs.has(record.streamId) || record.chatId !== context.chatId
            || record.characterId !== context.characterId || record.groupId !== context.groupId) continue;
        // ponytail: synchronous claim, not an atomic cross-tab lock; server fencing if strict multi-tab ownership matters.
        localStorage.removeItem(key);
        activeJobs.add(record.streamId);
        try {
            const response = await fetch(RESUME_URL, {
                method: 'POST', headers: getHeaders(),
                body: JSON.stringify({ stream_id: record.streamId, from: 0 }),
            });
            if (response.status === 404) { notify('expired', record); continue; }
            if (!response.ok) throw new Error(`Resume failed (${response.status})`);
            const result = await consume(response, record);
            await apply(result, record);
        } catch (error) {
            localStorage.setItem(key, JSON.stringify(record));
            notify('failed', record, error);
        } finally {
            activeJobs.delete(record.streamId);
        }
    }
}

export function isBackgroundGenerationAvailable() {
    return isServerEnv();
}

export function isBackgroundGenerationEnabled() {
    return isServerEnv() && accountStorage.getItem(STORAGE_KEY) === 'true';
}

export function setBackgroundGenerationEnabled(enabled) {
    accountStorage.setItem(STORAGE_KEY, String(Boolean(enabled)));
}

/** Resolves once the page is visible and online, or rejects on abort. */
function waitUntilForeground(signal) {
    const ready = () => document.visibilityState === 'visible' && navigator.onLine !== false;
    if (ready()) return Promise.resolve();
    return new Promise((resolve, reject) => {
        const check = () => {
            if (!ready()) return;
            cleanup();
            resolve();
        };
        const onAbort = () => {
            cleanup();
            reject(signal.reason ?? new DOMException('The operation was aborted.', 'AbortError'));
        };
        const cleanup = () => {
            document.removeEventListener('visibilitychange', check);
            window.removeEventListener('online', check);
            signal.removeEventListener('abort', onAbort);
        };
        document.addEventListener('visibilitychange', check);
        window.addEventListener('online', check);
        signal.addEventListener('abort', onAbort, { once: true });
    });
}

function cancelOnServer(streamId, stream) {
    const url = stream
        ? '/api/backends/chat-completions/cancel'
        : '/api/backends/chat-completions/cancel-generation';
    // keepalive: the Stop press must reach the server even if the page is closing.
    void fetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ stream_id: streamId }),
        keepalive: true,
    }).catch((error) => console.debug('Background generation cancel failed', error));
}

/**
 * Sends a generate request the server runs detached from this connection.
 *
 * Streaming returns a `{ body }` whose ReadableStream carries the same SSE
 * bytes as the plain endpoint, transparently re-attaching after drops, so the
 * existing parser stays unchanged. Non-streaming returns the fetch Response.
 *
 * @param {object} generateData Payload for the generate endpoint
 * @param {boolean} stream Whether the payload asks for streaming
 * @param {AbortSignal} signal Stop signal
 * @param {() => Record<string,string>} getHeaders Request headers factory
 */
export async function sendBackgroundGenerateRequest(generateData, stream, signal, getHeaders, context = {}) {
    const streamId = `bg-${uuidv4()}`;
    const key = JOB_PREFIX + streamId;
    const record = { ...context, streamId, stream, createdAt: Date.now(), source: generateData.chat_completion_source, model: generateData.model };
    localStorage.setItem(key, JSON.stringify(record));
    activeJobs.add(streamId);
    const complete = () => { localStorage.removeItem(key); activeJobs.delete(streamId); };
    const onAbort = () => {
        if (pageUnloading) return;
        complete();
        cancelOnServer(streamId, stream);
    };
    signal.addEventListener('abort', onAbort, { once: true });

    const post = (url, body) => fetch(url, {
        method: 'POST',
        headers: getHeaders(),
        body: JSON.stringify(body),
        signal,
    });

    // The first POST creates the job; repeating it with the same id attaches
    // rather than starting a second paid call.
    const open = async (from) => {
        let lastError;
        for (let attempt = 0; attempt < MAX_RESUME_ATTEMPTS; attempt++) {
            if (from !== null) await waitUntilForeground(signal);
            try {
                return from === null
                    ? await post(GENERATE_URL, { ...generateData, stream_id: streamId, background: true })
                    : await post(RESUME_URL, { stream_id: streamId, from });
            } catch (error) {
                if (signal.aborted) throw error;
                lastError = error;
                await new Promise((resolve) => setTimeout(resolve, Math.min(1000 * (attempt + 1), 5000)));
            }
        }
        throw lastError;
    };

    let response;
    try {
        response = await open(null);
    } catch (error) {
        signal.removeEventListener('abort', onAbort);
        activeJobs.delete(streamId);
        throw error;
    }
    if (!stream || !response.ok) {
        response.backgroundJobId = streamId;
        response.backgroundComplete = () => {
            signal.removeEventListener('abort', onAbort);
            complete();
        };
        if (!response.ok) response.backgroundComplete();
        return response;
    }

    // Counts complete SSE events (blank-line terminated) forwarded so far, so a
    // resume starts exactly after the last one the parser received.
    let delivered = 0;
    let pending = '';
    const decoder = new TextDecoder();
    const encoder = new TextEncoder();

    const body = new ReadableStream({
        async pull(controller) {
            try {
                while (true) {
                    try {
                        const reader = response.body.getReader();
                        while (true) {
                            const { done, value } = await reader.read();
                            if (done) break;
                            pending += decoder.decode(value, { stream: true });
                            const events = pending.split(/\r\n\r\n|\r\r|\n\n/g);
                            pending = events.pop();
                            for (const event of events) {
                                // Keep-alive comments carry no id; do not count them.
                                if (/^id:/m.test(event)) delivered++;
                                controller.enqueue(encoder.encode(`${event}\n\n`));
                                if (/^data: ?\[DONE\]$/m.test(event)) {
                                    signal.removeEventListener('abort', onAbort);
                                    controller.close();
                                    return;
                                }
                            }
                        }
                    } catch (error) {
                        if (signal.aborted) throw error;
                        console.info('Background generation connection dropped; will resume', error);
                    }
                    // Connection closed before [DONE]: re-attach from the last full event.
                    pending = '';
                    response = await open(delivered);
                    if (!response.ok) {
                        const text = await response.text();
                        throw new Error(`Background generation could not resume (${response.status}): ${text}`);
                    }
                }
            } catch (error) {
                signal.removeEventListener('abort', onAbort);
                activeJobs.delete(streamId);
                controller.error(error);
            }
        },
    });

    return { ok: true, status: 200, body, backgroundJobId: streamId, backgroundComplete: complete };
}
