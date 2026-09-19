import { isServerEnv } from '../../../tauri-bridge.js';
import { registerSystemRoutes } from './system-routes.js';
import { registerBootstrapRoutes } from './bootstrap-routes.js';
import { registerSettingsRoutes } from './settings-routes.js';
import { registerUserRoutes } from './user-routes.js';
import { registerExtensionRoutes } from './extensions-routes.js';
import { registerQuickReplyRoutes } from './quick-replies-routes.js';
import { registerResourceRoutes } from './resource-routes.js';
import { registerCharacterRoutes } from './character-routes.js';
import { registerChatRoutes } from './chat-routes.js';
import { registerBackupsRoutes } from './backups-routes.js';
import { registerAiRoutes } from './ai-routes.js';
import { registerProviderRoutes } from './provider-routes.js';
import { registerStatsRoutes } from './stats-routes.js';
import { registerWorldInfoRoutes } from './worldinfo-routes.js';
import { registerContentRoutes } from './content-routes.js';
import { registerAssetsRoutes } from './assets-routes.js';
import { registerSdRoutes } from './sd-routes.js';
import { registerTranslateRoutes } from './translate-routes.js';
import { registerTtsRoutes } from './tts-routes.js';
import { registerVectorRoutes } from './vector-routes.js';

/**
 * Endpoints the server host answers itself.
 *
 * Under the app these are emulated in the page because there is no HTTP server
 * to talk to. Under the server host they are real routes, and the interceptor
 * must not shadow them: `/api/bootstrap` and `/csrf-token` exist server-side,
 * chat read/save carry the revision token that the in-page emulation has no way
 * to produce, and generation must stream as SSE from the server rather than
 * being reassembled from an IPC channel.
 */
const SERVER_NATIVE_PREFIXES = Object.freeze([
    '/csrf-token',
    '/api/bootstrap',
    '/api/chats/get',
    '/api/chats/save',
    '/api/backends/chat-completions/',
]);

export function isServerNativeEndpoint(pathname) {
    return SERVER_NATIVE_PREFIXES.some((prefix) => (
        prefix.endsWith('/') ? pathname.startsWith(prefix) : pathname === prefix
    ));
}

export function registerRoutes(router, context, responses) {
    registerSystemRoutes(router, context, responses);
    registerBootstrapRoutes(router, context, responses);
    registerSettingsRoutes(router, context, responses);
    registerUserRoutes(router, context, responses);
    registerQuickReplyRoutes(router, context, responses);
    registerExtensionRoutes(router, context, responses);
    registerResourceRoutes(router, context, responses);
    registerCharacterRoutes(router, context, responses);
    registerChatRoutes(router, context, responses);
    registerBackupsRoutes(router, context, responses);
    registerContentRoutes(router, context, responses);
    registerAssetsRoutes(router, context, responses);
    registerWorldInfoRoutes(router, context, responses);
    registerAiRoutes(router, context, responses);
    registerVectorRoutes(router, context, responses);
    registerProviderRoutes(router, context, responses);
    registerSdRoutes(router, context, responses);
    registerTranslateRoutes(router, context, responses);
    registerTtsRoutes(router, context, responses);
    registerStatsRoutes(router, context, responses);
}

/**
 * True when the in-page router should answer this request.
 *
 * Server mode keeps the router for everything the server has no route for, so
 * upstream endpoints stay available without duplicating them twice.
 */
export function shouldHandleInPage(pathname) {
    return !(isServerEnv() && isServerNativeEndpoint(pathname));
}
