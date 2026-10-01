import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const source = await readFile(new URL('../src/scripts/tauri/setting/setting-panel/settings-view-model.js', import.meta.url), 'utf8');
const body = source.replace(/^import\s+[\s\S]*?;\n/gm, '').replace(/^export /gm, '');
const createModel = new Function('deps', `
    const { isMobile, isAndroidRuntime, isIosRuntime, isServerEnv,
        getActiveIosPolicyCapabilities, getRuntimePaths, getTauriTavernSettings,
        syncNativeRegexBackendEnabledFromSettings, createDataRootState,
        createTauriTavernSettingsState, isNativeRegexBackendEnabled } = deps;
    ${body}
    return { resolveTauriTavernSettingsCapabilities, loadTauriTavernSettingsViewModel };
`);

test('server settings do not request native runtime paths, including desktop-like browsers', async () => {
    let runtimePathCalls = 0;
    const model = createModel({
        isMobile: () => false,
        isAndroidRuntime: () => false,
        isIosRuntime: () => false,
        isServerEnv: () => true,
        getActiveIosPolicyCapabilities: () => null,
        getRuntimePaths: () => { runtimePathCalls++; throw new Error('native only'); },
        getTauriTavernSettings: async () => ({}),
        syncNativeRegexBackendEnabledFromSettings: () => {},
        createDataRootState: paths => paths,
        createTauriTavernSettingsState: settings => settings,
        isNativeRegexBackendEnabled: () => false,
    });
    const result = await model.loadTauriTavernSettingsViewModel();
    assert.equal(result.capabilities.supportsDataRootSelection, false);
    assert.equal(result.capabilities.supportsNativeDevLogs, false);
    assert.equal(result.dataRoot, null);
    assert.equal(runtimePathCalls, 0);
});

test('native data root selection excludes Android and desktop-like iPadOS', () => {
    for (const [android, ios, expected] of [[false, false, true], [true, false, false], [false, true, false]]) {
        const model = createModel({
            isMobile: () => false,
            isAndroidRuntime: () => android,
            isIosRuntime: () => ios,
            isServerEnv: () => false,
            getActiveIosPolicyCapabilities: () => null,
        });
        assert.equal(model.resolveTauriTavernSettingsCapabilities().supportsDataRootSelection, expected);
    }
});
