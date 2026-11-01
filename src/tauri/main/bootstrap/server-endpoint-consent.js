import { invoke } from '../../../tauri-bridge.js';

/** Install before the in-page router so both RPC and direct generation honor endpoint grants. */
export function installServerEndpointConsent(target = window) {
    const send = target.fetch.bind(target);
    target.fetch = async (input, init) => {
        const url = new URL(typeof input === 'string' || input instanceof URL ? input : input.url, target.location.href);
        if (url.origin !== target.location.origin) return send(input, init);
        // Request bodies may be streams; retain an untouched copy for the single authorized retry.
        const retry = input instanceof Request ? input.clone() : input;
        const response = await send(input, init);
        if (response.status !== 403 || url.pathname === '/rpc/grant_user_endpoint') return response;
        let failure;
        try { failure = await response.clone().json(); } catch { return response; }
        if (failure?.error !== 'endpoint_authorization_required' || typeof failure.endpoint !== 'string') return response;
        const endpoint = failure.endpoint;
        const warning = `Allow requests to ${endpoint}?\n\nThis endpoint may receive API keys, prompts and chat content. Only allow endpoints you recognize and trust.${endpoint.startsWith('http:') ? '\n\nHTTP is unencrypted: other devices on the network may observe or modify traffic.' : ''}`;
        if (!target.confirm(warning)) return response;
        await invoke('grant_user_endpoint', { endpoint, confirmed: true });
        return send(retry, init);
    };
}
