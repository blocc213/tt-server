// @ts-check

import { isServerEnv } from '../../../../tauri-bridge.js';
import { createPollingSubscribe } from './polling-stream.js';
import { createTauriEventStreamBridge } from './tauri-event-stream-bridge.js';

const BACKEND_LOG_EVENT = 'tauritavern-backend-log';
const DEFAULT_BACKEND_LOG_TAIL_LIMIT = 800;

/**
 * @param {unknown} value
 * @param {string} label
 */
function requirePositiveInteger(value, label) {
    const number = typeof value === 'number' ? value : Number(value);
    if (!Number.isSafeInteger(number) || number <= 0) {
        throw new Error(`${label} must be a positive integer`);
    }
    return number;
}

/**
 * @param {{ safeInvoke: (command: any, args?: any) => Promise<any> }} deps
 */
export function createBackendLogBridge({ safeInvoke }) {
    /** @param {number} limit */
    const tail = limit => safeInvoke('devlog_get_backend_log_tail', { limit });
    const subscribe = isServerEnv()
        ? createPollingSubscribe(() => tail(DEFAULT_BACKEND_LOG_TAIL_LIMIT))
        : createTauriEventStreamBridge({
            safeInvoke,
            enableCommand: 'devlog_set_backend_log_stream_enabled',
            eventName: BACKEND_LOG_EVENT,
        }).subscribe;

    return {
        /**
         * @param {{ limit?: number }} [options]
         */
        async tail(options = {}) {
            return tail(options?.limit === undefined
                ? DEFAULT_BACKEND_LOG_TAIL_LIMIT
                : requirePositiveInteger(options.limit, 'limit'));
        },
        subscribe,
    };
}
