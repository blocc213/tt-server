import { invoke, isServerEnv } from '../../../tauri-bridge.js';
import { stripJsonl } from '../../../tauri/main/kernel/chat-utils.js';
import {
    characterStemFromAvatarFileName,
    hasCharacterAvatarIdentity,
} from '../../../tauri/main/services/characters/character-identity.js';
import { fetchAssetStream } from './asset-io.js';
import { commitChatPayload } from './commit.js';
import { jsonlStreamToPayload } from './jsonl.js';

export const CHAT_COMMIT_REASON = Object.freeze({
    MUTATION: 'mutation',
    PROVIDER_BARRIER: 'providerBarrier',
    GENERATION_CHECKPOINT: 'generationCheckpoint',
    MAINTENANCE: 'maintenance',
});

export function normalizeChatFileName(fileName) {
    return stripJsonl(fileName);
}

/**
 * Chat folders are keyed by the character avatar filename stem.
 * avatarUrl is SillyTavern's avatar_url API field, not a browser asset URL.
 */
export function resolveCharacterDirectoryId(characterName, avatarUrl) {
    if (hasCharacterAvatarIdentity(avatarUrl)) {
        return characterStemFromAvatarFileName(avatarUrl, 'avatar_url', { required: true });
    }

    return String(characterName || '').trim();
}

/// Tracks the chat revision each loaded chat came from.
///
/// A save must prove it is replacing the revision it loaded, otherwise a tab
/// left open on an old device would overwrite newer content saved elsewhere.
/// Keyed by "<characterDirectory>/<fileName>"; cleared on reload with the page.
const loadedChatVersions = new Map();

function versionKey(characterId, fileName) {
    return `character:${characterId}/${fileName}`;
}

function groupVersionKey(chatId) {
    return `group:${chatId}`;
}

export function getLoadedChatVersion(characterId, fileName) {
    return loadedChatVersions.get(versionKey(characterId, fileName)) ?? null;
}

export function forgetLoadedChatVersion(characterId, fileName) {
    loadedChatVersions.delete(versionKey(characterId, fileName));
}
export function setLoadedChatVersion(characterId, fileName, version) {
    const key = versionKey(characterId, fileName);
    if (version) {
        loadedChatVersions.set(key, version);
    } else {
        loadedChatVersions.delete(key);
    }
}

async function requestServerChat(path, body) {
    const response = await fetch(path, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        cache: 'no-store',
        body: JSON.stringify(body),
    });

    const text = await response.text();
    const payload = text ? JSON.parse(text) : null;
    if (!response.ok) {
        const error = new Error(payload?.error || `Request failed: ${path}`);
        // Preserve upstream's integrity signal so the existing overwrite prompt
        // keeps working unchanged.
        if (payload?.error === 'integrity') {
            error.code = 'integrity';
        }
        throw error;
    }

    return { payload, headers: response.headers };
}

// Chat reads keep upstream's bare-array body; the revision rides in a header.
const CHAT_VERSION_HEADER = 'x-tauritavern-chat-version';

export async function loadCharacterChatPayload({ characterName, avatarUrl, fileName, allowNotFound = false }) {
    const normalizedCharacter = resolveCharacterDirectoryId(characterName, avatarUrl);
    const normalizedFile = normalizeChatFileName(fileName);

    if (!normalizedCharacter || !normalizedFile.trim()) {
        throw new Error('Invalid character chat payload request');
    }

    if (isServerEnv()) {
        const { payload, headers } = await requestServerChat('/api/chats/get', {
            ch_name: characterName,
            avatar_url: avatarUrl,
            file_name: normalizedFile,
            allow_not_found: allowNotFound,
        });

        const key = versionKey(normalizedCharacter, normalizedFile);
        const version = headers.get(CHAT_VERSION_HEADER);
        if (version) {
            loadedChatVersions.set(key, JSON.parse(version));
        } else {
            // No file yet: the next save must create it rather than replace one.
            loadedChatVersions.delete(key);
        }

        return payload;
    }

    const path = await invoke('get_chat_payload_path', {
        characterName: normalizedCharacter,
        fileName: normalizedFile,
        allowNotFound,
    });

    if (!path) {
        if (allowNotFound) {
            return [];
        }
        throw new Error('Chat payload path is empty');
    }

    const stream = await fetchAssetStream(path);
    return jsonlStreamToPayload(stream);
}

export async function saveCharacterChatPayload({ characterName, avatarUrl, fileName, payload, force = false, commitReason = CHAT_COMMIT_REASON.MUTATION }) {
    const normalizedCharacter = resolveCharacterDirectoryId(characterName, avatarUrl);
    const normalizedFile = normalizeChatFileName(fileName);
    if (!Array.isArray(payload) || payload.length === 0 || !normalizedCharacter || !normalizedFile.trim()) {
        throw new Error('Invalid chat payload');
    }

    if (isServerEnv()) {
        const key = versionKey(normalizedCharacter, normalizedFile);
        const version = loadedChatVersions.get(key) ?? null;
        const { payload: result } = await requestServerChat('/api/chats/save', {
            ch_name: characterName,
            avatar_url: avatarUrl,
            file_name: normalizedFile,
            chat: payload,
            force,
            version,
            is_new: !version,
        });

        // Track the revision just written so the next save from this page is not
        // mistaken for a stale one.
        if (result?.version) {
            loadedChatVersions.set(key, result.version);
        }
        return;
    }

    await commitChatPayload({
        target: {
            kind: 'character',
            characterId: normalizedCharacter,
            fileName: normalizedFile,
        },
        payload,
        force,
        commitReason,
    });
}

/**
 * Server mode: upstream SillyTavern extensions POST `/api/chats/save` without
 * TT's `version`/`is_new`, which the server refuses. For the chat this page has
 * open, attach the revision the page loaded so the server's stale-write check
 * still applies. Returns null to leave the request untouched.
 *
 * @param {{ url: URL, input: unknown, init: RequestInit, send: typeof fetch, activeTarget: () => ({ characterId: string, fileName: string } | null) }} args
 * @returns {Promise<Response> | null}
 */
export function adaptLegacyServerChatSave({ url, input, init, send, activeTarget }) {
    if (!isServerEnv() || url.pathname !== '/api/chats/save' || typeof input !== 'string' && !(input instanceof URL)) {
        return null;
    }
    if (String(init?.method || 'GET').toUpperCase() !== 'POST' || typeof init?.body !== 'string') {
        return null;
    }
    let body;
    try {
        body = JSON.parse(init.body);
    } catch {
        return null;
    }
    if (!body || typeof body !== 'object' || 'version' in body || 'is_new' in body || body.force === true) {
        return null;
    }

    const characterId = resolveCharacterDirectoryId(body.ch_name, body.avatar_url);
    const fileName = normalizeChatFileName(body.file_name);
    const active = activeTarget();
    if (!characterId || !fileName.trim() || active?.characterId !== characterId || normalizeChatFileName(active.fileName) !== fileName) {
        return null;
    }
    const key = versionKey(characterId, fileName);
    const version = loadedChatVersions.get(key);
    if (!version) {
        return null;
    }

    return send(input, { ...init, body: JSON.stringify({ ...body, version, is_new: false }) }).then(async (response) => {
        if (response.ok) {
            const saved = await response.clone().json().catch(() => null);
            // Advance only from the revision we sent; a newer save may have landed meanwhile.
            if (saved?.version && loadedChatVersions.get(key) === version) {
                loadedChatVersions.set(key, saved.version);
            }
        }
        return response;
    });
}

export async function loadGroupChatPayload({ id, allowNotFound = false }) {
    const normalizedId = normalizeChatFileName(id);
    if (!normalizedId.trim()) {
        throw new Error('Invalid group chat payload request');
    }

    if (isServerEnv()) {
        const { payload, headers } = await requestServerChat('/api/tauritavern/group-chats/get', {
            id: normalizedId,
            allow_not_found: allowNotFound,
        });
        const key = groupVersionKey(normalizedId);
        const version = headers.get(CHAT_VERSION_HEADER);
        if (version) {
            loadedChatVersions.set(key, JSON.parse(version));
        } else {
            loadedChatVersions.delete(key);
        }
        return payload;
    }
    const path = await invoke('get_group_chat_path', {
        id: normalizedId,
        allowNotFound,
    });

    if (!path) {
        if (allowNotFound) {
            return [];
        }
        throw new Error('Group chat payload path is empty');
    }

    const stream = await fetchAssetStream(path);
    return jsonlStreamToPayload(stream);
}

export async function saveGroupChatPayload({ id, payload, force = false, commitReason = CHAT_COMMIT_REASON.MUTATION }) {
    const normalizedId = normalizeChatFileName(id);
    if (!Array.isArray(payload) || payload.length === 0 || !normalizedId.trim()) {
        throw new Error('Invalid group chat payload');
    }

    if (isServerEnv()) {
        const key = groupVersionKey(normalizedId);
        const version = loadedChatVersions.get(key) ?? null;
        const { payload: result } = await requestServerChat('/api/tauritavern/group-chats/save', {
            id: normalizedId,
            chat: payload,
            force,
            version,
            is_new: !version,
        });
        if (result?.version) {
            loadedChatVersions.set(key, result.version);
        }
        return;
    }
    await commitChatPayload({
        target: {
            kind: 'group',
            chatId: normalizedId,
        },
        payload,
        force,
        commitReason,
    });
}
