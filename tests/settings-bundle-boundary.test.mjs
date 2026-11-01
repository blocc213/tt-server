import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

test('settings sibling bundles remain runtime imports independent of generated output', async () => {
    for (const [module, bundle] of [
        ['setting-panel/sync-popup.js', 'sync'],
        ['setting-panel/sync-listeners.js', 'sync'],
        ['setting-panel/settings-popup.js', 'settings'],
        ['dev-logs.js', 'dev-logs'],
    ]) {
        const source = await readFile(new URL(`../src/scripts/tauri/setting/${module}`, import.meta.url), 'utf8');
        assert.ok(source.includes(`import('/scripts/tauri/setting/dist/${bundle}.bundle.js' /* webpackIgnore: true */)`));
        assert.doesNotMatch(source, /import\(new URL\([^\n]+\.bundle\.js/);
    }
    const view = await readFile(new URL('../src/scripts/tauri/setting/settings-app/SettingsApp.tsx', import.meta.url), 'utf8');
    assert.doesNotMatch(view, /from ['"][^'"]*(?:background-generation|AccountStorage|script|utils)\.js/);
    assert.match(view, /actions\.backgroundGeneration/);
});

test('server settings Save waits for the account preference commit and keeps popup open on failure', async () => {
    const source = await readFile(new URL('../src/scripts/tauri/setting/setting-panel/settings-popup.js', import.meta.url), 'utf8');
    const start = source.indexOf('if (isBackgroundGenerationAvailable() && !await saveSettings())');
    const end = source.indexOf('if (update.hasChanges)', start);
    assert.ok(start > 0 && end > start);
    const savePreference = new Function('isBackgroundGenerationAvailable', 'saveSettings', `return (async () => { ${source.slice(start, end)} })();`);
    let release;
    const pending = new Promise(resolve => { release = resolve; });
    let closed = false;
    const saving = savePreference(() => true, () => pending).then(() => { closed = true; });
    await Promise.resolve();
    assert.equal(closed, false);
    release(true);
    await saving;
    assert.equal(closed, true);
    await assert.rejects(savePreference(() => true, async () => false), /preference was not saved/);
});
