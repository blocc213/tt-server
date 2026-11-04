// @ts-check

const POLL_INTERVAL_MS = 1000;

/**
 * Server-mode stand-in for Tauri event streams: the browser has no host event
 * channel, so new log entries are found by polling an id-ordered tail.
 * Entries present when subscribing are the caller's initial snapshot and are
 * not dispatched again.
 *
 * @template {{ id: number }} T
 * @param {() => Promise<T[]>} fetchTail
 * @param {number} [intervalMs]
 */
export function createPollingSubscribe(fetchTail, intervalMs = POLL_INTERVAL_MS) {
    /**
     * @param {(entry: T) => void} handler
     * @returns {Promise<() => Promise<void>>}
     */
    return async function subscribe(handler) {
        if (typeof handler !== 'function') {
            throw new Error('handler must be a function');
        }

        let lastId = Math.max(0, ...(await fetchTail()).map(entry => entry.id));
        let stopped = false;
        /** @type {ReturnType<typeof setTimeout> | undefined} */
        let timer;

        async function poll() {
            try {
                for (const entry of await fetchTail()) {
                    if (stopped) return;
                    if (entry.id > lastId) {
                        lastId = entry.id;
                        handler(entry);
                    }
                }
            } catch (error) {
                // A transient network failure must not end the live view; the
                // next poll catches up from lastId.
                console.warn('[TauriTavern] Log poll failed:', error);
            }
            if (!stopped) timer = setTimeout(poll, intervalMs);
        }

        timer = setTimeout(poll, intervalMs);
        return async () => {
            stopped = true;
            clearTimeout(timer);
        };
    };
}
