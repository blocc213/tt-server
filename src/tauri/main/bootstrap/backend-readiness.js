// @ts-check

import { invoke } from '../../../tauri-bridge.js';

export async function waitForBackendReady() {
    // Goes through the shared bridge so it works under both hosts: Tauri IPC in
    // the app, POST /rpc/wait_for_backend_ready on the server.
    await invoke('wait_for_backend_ready');
}
