// Core Tauri bridge for frontend modules.

import { SILLYTAVERN_COMPAT_VERSION } from './compat-version.js';

function detectTauriEnv() {
    if (typeof window === 'undefined') {
        return false;
    }

    // __TAURI_RUNNING__ is set in init.js before dynamic imports to avoid
    // startup races where mobile bridge internals are injected a bit later.
    return window.__TAURI_RUNNING__ === true
        || window.__TAURI_INTERNALS__ !== undefined
        || typeof window.__TAURI__?.core?.invoke === 'function';
}

/// True when the page is served over HTTP by tauritavern-server.
function detectServerEnv() {
    return typeof window !== 'undefined' && window.__TAURITAVERN_SERVER__ === true;
}

export const isTauriEnv = detectTauriEnv();

/// True when the page is served over HTTP by tauritavern-server.
///
/// A function, not a constant: init.js assigns the flag during bootstrap, so a
/// value captured at module-evaluation time can be read too early.
export function isServerEnv() {
    return detectServerEnv();
}

function getTauri() {
    if (typeof window === 'undefined') {
        return null;
    }

    return window.__TAURI__ || null;
}

function getInvokeFn() {
    const fn = getTauri()?.core?.invoke;
    return typeof fn === 'function' ? fn : null;
}

function isPlainObject(value) {
    if (Object.prototype.toString.call(value) !== '[object Object]') {
        return false;
    }
    const prototype = Object.getPrototypeOf(value);
    return prototype === null || prototype === Object.prototype;
}

function withTauriArgumentAliases(args) {
    if (!isPlainObject(args)) {
        return args;
    }

    const aliased = { ...args };
    for (const [key, value] of Object.entries(args)) {
        if (!key.includes('_')) {
            continue;
        }

        const camelCaseKey = key.replace(/_+([a-zA-Z0-9])/g, (_, char) => char.toUpperCase());
        if (!Object.prototype.hasOwnProperty.call(aliased, camelCaseKey)) {
            aliased[camelCaseKey] = value;
        }
    }

    return aliased;
}

/// Status -> the message prefix `resolveHostErrorResponse` parses back.
///
/// Mirrors `CommandError`'s `#[error("...")]` strings so the in-page router
/// produces the same status under both hosts. Already-prefixed messages pass
/// through untouched: the server's own text sometimes carries it.
const ERROR_CATEGORY_PREFIXES = Object.freeze({
    400: 'Bad request',
    401: 'Unauthorized',
    403: 'Permission denied',
    404: 'Not found',
    409: 'Conflict',
    429: 'Too many requests',
});

function prefixErrorCategory(status, message) {
    const prefix = ERROR_CATEGORY_PREFIXES[status];
    if (!prefix) {
        return message;
    }

    const lower = message.toLowerCase();
    for (const candidate of Object.values(ERROR_CATEGORY_PREFIXES)) {
        if (lower.startsWith(`${candidate.toLowerCase()}:`)) {
            return message;
        }
    }

    return `${prefix}: ${message}`;
}

/// Calls a backend command over HTTP in server mode.
///
/// The server exposes an explicit allowlist, so an unknown command answers 404
/// and surfaces here as a normal error instead of hanging.
async function invokeOverHttp(command, args, options) {
    const rawBody = args instanceof Uint8Array || args instanceof ArrayBuffer;
    const url = rawBody
        ? `/rpc-raw/${encodeURIComponent(command)}`
        : `/rpc/${encodeURIComponent(command)}`;
    const headers = rawBody
        ? { ...(options?.headers || {}) }
        : { 'Content-Type': 'application/json' };
    const response = await fetch(url, {
        method: 'POST',
        headers,
        cache: 'no-store',
        body: rawBody ? args : JSON.stringify(args ?? {}),
    });

    const text = await response.text();
    const payload = text ? JSON.parse(text) : null;

    if (!response.ok) {
        const message = payload?.error || `Command failed: ${command}`;
        // Re-apply the category prefix the Tauri host's `CommandError` Display
        // emits (`presentation/errors.rs`). The server carries the category in
        // the HTTP status instead, and the in-page router maps errors back to
        // statuses by parsing that prefix
        // (`tauri/main/kernel/host-error-response.js:resolveHostErrorResponse`).
        // Without it every server-mode failure collapsed to 500, so callers that
        // key off a specific status -- the settings 409 conflict prompt, the
        // 404-tolerant delete paths -- never saw it.
        throw new Error(prefixErrorCategory(response.status, message));
    }

    return payload;
}


export const invoke = (...args) => {
    const [command, commandArgs] = args;
    const normalizedArgs = args.length >= 2 && isPlainObject(commandArgs)
        ? withTauriArgumentAliases(commandArgs)
        : commandArgs;

    // Re-checked per call rather than captured at module load: the flag is set
    // by init.js, and tests swap the host between imports.
    if (detectServerEnv()) {
        return invokeOverHttp(command, normalizedArgs, args[2]);
    }

    const fn = getTauri()?.core?.invoke;
    if (typeof fn !== 'function') {
        throw new Error('Tauri invoke is unavailable');
    }

    if (args.length === 2 && isPlainObject(args[1])) {
        return fn(command, normalizedArgs);
    }

    return fn(...args);
};

function getHostSafeInvoke() {
    if (typeof window === 'undefined') {
        return null;
    }

    const fn = window.__TAURITAVERN__?.invoke?.safeInvoke;
    return typeof fn === 'function' ? fn : null;
}

async function invokeWithHostNormalization(command, args) {
    const safeInvoke = getHostSafeInvoke();
    if (safeInvoke) {
        return safeInvoke(command, args);
    }

    return args === undefined ? invoke(command) : invoke(command, args);
}

export const listen = (...args) => {
    const fn = getTauri()?.event?.listen;
    if (typeof fn !== 'function') {
        throw new Error('Tauri listen is unavailable');
    }
    return fn(...args);
};

export function createChannel(onmessage) {
    const Channel = getTauri()?.core?.Channel;
    if (typeof Channel !== 'function') {
        throw new Error('Tauri Channel is unavailable');
    }

    return new Channel(onmessage);
}

export const convertFileSrc = (path, protocol = 'asset') => {
    const fn = getTauri()?.core?.convertFileSrc;
    if (typeof fn !== 'function') {
        throw new Error('Tauri convertFileSrc is unavailable');
    }
    return fn(path, protocol);
};

export function isTauri() {
    return detectTauriEnv();
}

/// True when a TauriTavern backend is reachable, app or server.
export function hasBackend() {
    return detectTauriEnv() || detectServerEnv();
}

export async function initializeBridge() {
    if (!detectServerEnv()) {
        const invokeFn = getInvokeFn();
        if (!invokeFn) {
            return false;
        }
    }

    try {
        return await invoke('is_ready');
    } catch (error) {
        console.error('Failed to initialize the TauriTavern backend bridge:', error);
        return false;
    }
}

export async function getClientVersion() {
    try {
        return await invoke('get_client_version');
    } catch (error) {
        console.error('Error getting client version from Tauri backend:', error);
        const version = await invoke('get_version');
        return {
            agent: `SillyTavern:${SILLYTAVERN_COMPAT_VERSION}:TauriTavern`,
            pkgVersion: SILLYTAVERN_COMPAT_VERSION,
            tauriVersion: version,
            gitRevision: null,
            gitBranch: null,
            defaultUpdateChannel: 'stable',
        };
    }
}

export async function checkForUpdate(channel) {
    return invokeWithHostNormalization('check_for_update', { channel });
}

export async function getTauriTavernSettings() {
    return invoke('get_tauritavern_settings');
}

export async function getChatBackupStorageStats() {
    const invokeFn = getInvokeFn();
    if (!invokeFn) {
        throw new Error('Tauri invoke is unavailable');
    }

    return invokeFn('get_chat_backup_storage_stats');
}

export async function updateTauriTavernSettings(dto) {
    if (!isPlainObject(dto)) {
        throw new Error('Invalid TauriTavern settings DTO');
    }

    return invoke('update_tauritavern_settings', { dto });
}

export async function getRuntimePaths() {
    const invokeFn = getInvokeFn();
    if (!invokeFn) {
        throw new Error('Tauri invoke is unavailable');
    }

    return invokeFn('get_runtime_paths');
}

export async function setDataRoot(dataRoot) {
    if (typeof dataRoot !== 'string' || dataRoot.trim() === '') {
        throw new Error('Invalid data root path');
    }
    return invokeWithHostNormalization('set_data_root', { data_root: dataRoot });
}

export async function openDialog(options = {}) {
    const invokeFn = getInvokeFn();
    if (!invokeFn) {
        throw new Error('Tauri invoke is unavailable');
    }

    if (!isPlainObject(options)) {
        throw new Error('Invalid dialog options: expected an object');
    }

    Object.freeze(options);

    return invokeWithHostNormalization('plugin:dialog|open', { options });
}

function normalizeExternalUrl(url) {
    const value = String(url instanceof URL ? url.href : url ?? '').trim();
    if (!value) {
        throw new Error('External URL is required');
    }

    try {
        return new URL(value, window.location.href).toString();
    } catch {
        throw new Error(`Invalid external URL: ${value}`);
    }
}

export async function openExternalUrl(url, openWith) {
    const href = normalizeExternalUrl(url);
    const invokeFn = getInvokeFn();

    if (invokeFn) {
        return invokeFn('plugin:opener|open_url', {
            url: href,
            with: openWith,
        });
    }

    const openedWindow = typeof window.open === 'function'
        ? window.open(href, '_blank', 'noopener,noreferrer')
        : null;

    if (openedWindow) {
        return;
    }

    if (typeof window.location?.assign === 'function') {
        window.location.assign(href);
        return;
    }

    throw new Error('Unable to open external URL');
}
