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

    return payload;
}

export async function loadCharacterChatPayload({ characterName, avatarUrl, fileName, allowNotFound = false }) {
    const normalizedCharacter = resolveCharacterDirectoryId(characterName, avatarUrl);
    const normalizedFile = normalizeChatFileName(fileName);

    if (!normalizedCharacter || !normalizedFile.trim()) {
        throw new Error('Invalid character chat payload request');
    }

    if (isServerEnv()) {
        const result = await requestServerChat('/api/chats/get', {
            ch_name: characterName,
            avatar_url: avatarUrl,
            file_name: normalizedFile,
            allow_not_found: allowNotFound,
        });

        const key = versionKey(normalizedCharacter, normalizedFile);
        if (result?.version) {
            loadedChatVersions.set(key, result.version);
        } else {
            // No file yet: the next save must create it rather than replace one.
            loadedChatVersions.delete(key);
        }

        return Array.isArray(result) ? result : (result?.chat ?? []);
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
        const result = await requestServerChat('/api/chats/save', {
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

export async function loadGroupChatPayload({ id, allowNotFound = false }) {
    const normalizedId = normalizeChatFileName(id);
    if (!normalizedId.trim()) {
        throw new Error('Invalid group chat payload request');
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
