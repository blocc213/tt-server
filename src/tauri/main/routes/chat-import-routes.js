import { extractErrorText, resolveHostErrorResponse } from '../kernel/host-error-response.js';
import { resolveRouteCharacterId } from './character-route-utils.js';

function hostErrorResponse(jsonResponse, error) {
    const resolved = resolveHostErrorResponse(extractErrorText(error));
    return jsonResponse({ error: resolved.body }, resolved.status);
}

function importedFileNames(value) {
    if (!Array.isArray(value) || value.length === 0 || !value.every(name => typeof name === 'string' && name)) {
        throw new Error('Chat import returned no file names');
    }
    return value;
}

function importedGroupChatId(value) {
    const chatId = typeof value === 'string' ? value.trim() : '';
    if (!chatId) {
        throw new Error('Group chat import returned no chat id');
    }
    return chatId;
}

export function registerChatImportRoutes(router, context, { jsonResponse }) {
    router.post('/api/chats/import', async ({ body }) => {
        if (!(body instanceof FormData)) {
            return jsonResponse({ error: 'Expected multipart form data' }, 400);
        }

        const backupName = String(body.get('backup_name') || '').trim();
        const file = body.get('avatar');
        const restoreFromBackup = Boolean(backupName) && !(file instanceof Blob);
        if (!restoreFromBackup && !(file instanceof Blob)) {
            return jsonResponse({ error: 'No chat file provided' }, 400);
        }

        const fileType = String(body.get('file_type') || '').trim().toLowerCase();
        if (!restoreFromBackup && !['json', 'jsonl'].includes(fileType)) {
            return jsonResponse({ error: `Unsupported chat import format: ${fileType || '(none)'}` }, 400);
        }

        const characterDisplayName = String(body.get('character_name') || '').trim();
        const resolved = await resolveRouteCharacterId(context, {
            avatar: body.get('avatar_url'),
            fallbackName: characterDisplayName,
        });
        if (resolved.responseBody) {
            return jsonResponse(resolved.responseBody, 400);
        }
        const characterId = resolved.characterId;
        if (!characterId) {
            return jsonResponse({ error: 'Unable to resolve the target character for chat import' }, 400);
        }

        if (restoreFromBackup) {
            try {
                const fileNames = await context.safeInvoke('restore_character_chat_backup', {
                    dto: {
                        backup_name: backupName,
                        character_name: characterId,
                        character_display_name: characterDisplayName || characterId,
                    },
                });

                return jsonResponse({ res: true, fileNames: importedFileNames(fileNames) });
            } catch (error) {
                return hostErrorResponse(jsonResponse, error);
            }
        }

        const preferredName = file instanceof File && file.name ? file.name : `import.${fileType}`;
        const fileInfo = await context.materializeUploadFile(file, {
            kind: 'chat-import',
            preferredName,
            preferredExtension: fileType,
        });
        if (!fileInfo?.filePath) {
            const reason = fileInfo?.error ? `: ${fileInfo.error}` : '';
            return jsonResponse({ error: `Unable to access uploaded chat file path${reason}` }, 400);
        }

        try {
            const fileNames = await context.safeInvoke('import_character_chats', {
                dto: {
                    character_name: characterId,
                    character_display_name: characterDisplayName || null,
                    user_name: String(body.get('user_name') || '').trim() || null,
                    file_path: fileInfo.filePath,
                    file_type: fileType,
                },
            });

            return jsonResponse({ res: true, fileNames: importedFileNames(fileNames) });
        } catch (error) {
            return hostErrorResponse(jsonResponse, error);
        } finally {
            await fileInfo.cleanup?.();
        }
    });

    router.post('/api/chats/group/import', async ({ body }) => {
        if (!(body instanceof FormData)) {
            return jsonResponse({ error: 'Expected multipart form data' }, 400);
        }

        const backupName = String(body.get('backup_name') || '').trim();
        const file = body.get('avatar');
        const restoreFromBackup = Boolean(backupName) && !(file instanceof Blob);
        if (restoreFromBackup) {
            try {
                const chatId = await context.safeInvoke('restore_group_chat_backup', {
                    dto: { backup_name: backupName },
                });
                return jsonResponse({ res: importedGroupChatId(chatId) });
            } catch (error) {
                return hostErrorResponse(jsonResponse, error);
            }
        }

        if (!(file instanceof Blob)) {
            return jsonResponse({ error: 'No group chat file provided' }, 400);
        }

        const preferredName = file instanceof File && file.name ? file.name : 'group-chat.jsonl';
        const fileInfo = await context.materializeUploadFile(file, {
            kind: 'chat-import',
            preferredName,
            preferredExtension: 'jsonl',
        });
        if (!fileInfo?.filePath) {
            const reason = fileInfo?.error ? `: ${fileInfo.error}` : '';
            return jsonResponse({ error: `Unable to access uploaded group chat file path${reason}` }, 400);
        }

        try {
            const chatId = await context.safeInvoke('import_group_chat_payload', {
                dto: { file_path: fileInfo.filePath },
            });
            return jsonResponse({ res: importedGroupChatId(chatId) });
        } catch (error) {
            return hostErrorResponse(jsonResponse, error);
        } finally {
            await fileInfo.cleanup?.();
        }
    });
}
