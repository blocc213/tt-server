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
export async function sendBackgroundGenerateRequest(generateData, stream, signal, getHeaders) {
    const streamId = `bg-${uuidv4()}`;
    const onAbort = () => cancelOnServer(streamId, stream);
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
            await waitUntilForeground(signal);
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
        throw error;
    }
    if (!stream || !response.ok) {
        signal.removeEventListener('abort', onAbort);
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
                controller.error(error);
            }
        },
    });

    return { ok: true, status: 200, body };
}
