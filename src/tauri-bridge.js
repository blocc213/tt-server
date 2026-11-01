// Core Tauri bridge for frontend modules.

function getTauri() {
    if (typeof window === 'undefined') {
        return null;
    }

    return window.__TAURI__ || null;
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

export function isServerEnv() {
    return globalThis.__TAURITAVERN_HOST__?.kind === 'server'
        || globalThis.__TAURITAVERN_SERVER__ === true
        || globalThis.window?.__TAURITAVERN_SERVER__ === true;
}

export function isTauri() { return typeof getTauri()?.core?.invoke === 'function' && !isServerEnv(); }
export function hasBackend() { return isTauri() || isServerEnv(); }

const loadedChatVersions = new Map();
const chatCommitTargets = new Map();
const serverResources = new Set();
let clientId;
const targetKey = target => target.kind === 'group' ? `group:${target.chatId}` : `character:${target.characterId}/${target.fileName}`;

async function invokeHttp(command, args, options = {}) {
    clientId ??= Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, '0')).join('');
    if (command === 'plugin:resources|close') command = 'close_resource';
    if (['subscribe_agent_run_live_projection', 'register_agent_extension_tool', 'set_agent_extension_tool_enabled', 'resolve_agent_extension_tool_call'].includes(command)) {
        throw new Error('Agent live projection and browser extension tools are not available on server host; use a native host.');
    }
    if (command === 'begin_chat_commit') {
        const version = loadedChatVersions.get(targetKey(args.target)) ?? null;
        args = { ...args, version, isNew: !version, force: args.operation?.force === true };
    }
    const raw = args instanceof ArrayBuffer || ArrayBuffer.isView(args);
    const headers = new Headers(options.headers);
    headers.set('x-tauritavern-client-id', clientId);
    headers.set('Content-Type', raw ? 'application/octet-stream' : 'application/json');
    const response = await fetch(`${raw ? '/rpc-raw/' : '/rpc/'}${encodeURIComponent(command)}`, {
        method: 'POST', credentials: 'same-origin', headers,
        body: raw ? args : JSON.stringify(withTauriArgumentAliases(args ?? {})),
        signal: options.signal,
    });
    if (!response.ok) {
        const text = await response.text();
        let payload;
        try { payload = JSON.parse(text); } catch { payload = null; }
        const error = new Error(payload?.message || payload?.error || text || response.statusText);
        Object.assign(error, { status: response.status, payload });
        if (response.status === 409 && command.includes('chat_commit')) error.code = 'integrity';
        throw error;
    }
    if (response.headers.get('content-type')?.includes('application/octet-stream')) {
        const bytes = await response.arrayBuffer();
        if (command === 'read_bytes' && bytes.byteLength === 0) serverResources.delete(args.rid);
        return bytes;
    }
    const result = response.status === 204 ? undefined : await response.json();
    if (command === 'open_cold_chat' || command === 'open_chat_payload_json') {
        const header = response.headers.get('x-tauritavern-chat-version');
        loadedChatVersions.set(targetKey(args.target), header ? JSON.parse(header) : result?.version ?? null);
    }
    if (command === 'begin_chat_commit') chatCommitTargets.set(result.sessionId, args.target);
    if (command === 'finish_chat_commit') {
        const target = chatCommitTargets.get(args.sessionId);
        if (target && result?.version) loadedChatVersions.set(targetKey(target), result.version);
        chatCommitTargets.delete(args.sessionId);
    }
    if (command === 'abort_chat_commit') chatCommitTargets.delete(args.sessionId);
    if (command === 'close_resource') serverResources.delete(args.rid);
    if (command === 'open_cold_chat' && result) { serverResources.add(result.readerId); serverResources.add(result.sourceId); }
    else if (command.startsWith('open_') && Number.isInteger(result)) serverResources.add(result);
    return result;
}

if (typeof globalThis.window?.addEventListener === 'function') {
    window.addEventListener('pagehide', () => {
        if (!isServerEnv() || !clientId) return;
        for (const rid of serverResources) {
            void fetch('/rpc/close_resource', {
                method: 'POST', credentials: 'same-origin', keepalive: true,
                headers: { 'Content-Type': 'application/json', 'x-tauritavern-client-id': clientId },
                body: JSON.stringify({ rid }),
            }).catch(() => {});
        }
        serverResources.clear();
    });
}

export const invoke = (...args) => {
    if (isServerEnv()) return invokeHttp(...args);
    const fn = getTauri()?.core?.invoke;
    if (typeof fn !== 'function') {
        throw new Error('Tauri invoke is unavailable');
    }

    if (args.length === 2 && isPlainObject(args[1])) {
        return fn(args[0], withTauriArgumentAliases(args[1]));
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

export function getCurrentWindow() {
    const getCurrentWindow = getTauri()?.window?.getCurrentWindow;
    if (typeof getCurrentWindow !== 'function') {
        throw new Error('Tauri window API is unavailable');
    }
    return getCurrentWindow();
}

// Optional plugin: absence is a valid state, not an error.
export function getBarcodeScanner() {
    return getTauri()?.barcodeScanner;
}

export const convertFileSrc = (path, protocol = 'asset') => {
    const fn = getTauri()?.core?.convertFileSrc;
    if (typeof fn !== 'function') {
        throw new Error('Tauri convertFileSrc is unavailable');
    }
    return fn(path, protocol);
};

export async function getClientVersion() {
    return invoke('get_client_version');
}

export async function checkForUpdate(channel) {
    return invokeWithHostNormalization('check_for_update', { channel });
}

export async function getTauriTavernSettings() {
    return invoke('get_tauritavern_settings');
}

export async function getChatBackupStorageStats() {
    return invoke('get_chat_backup_storage_stats');
}

export async function updateTauriTavernSettings(dto) {
    if (!isPlainObject(dto)) {
        throw new Error('Invalid TauriTavern settings DTO');
    }

    return invokeWithHostNormalization('update_tauritavern_settings', { dto });
}

export async function getRuntimePaths() {
    return invoke('get_runtime_paths');
}

export async function setDataRoot(dataRoot) {
    if (typeof dataRoot !== 'string' || dataRoot.trim() === '') {
        throw new Error('Invalid data root path');
    }
    return invokeWithHostNormalization('set_data_root', { data_root: dataRoot });
}

export async function openDialog(options = {}) {
    if (!isPlainObject(options)) {
        throw new Error('Invalid dialog options: expected an object');
    }

    Object.freeze(options);

    return invokeWithHostNormalization('plugin:dialog|open', { options });
}

export function writeClipboardText(text) {
    if (isServerEnv()) return navigator.clipboard.writeText(text);
    return invoke('plugin:clipboard-manager|write_text', { text });
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
    if (isServerEnv()) {
        window.open(normalizeExternalUrl(url), '_blank', 'noopener,noreferrer');
        return;
    }
    const href = normalizeExternalUrl(url);
    return invoke('plugin:opener|open_url', {
        url: href,
        with: openWith,
    });
}
