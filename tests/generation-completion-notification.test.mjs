import test from 'node:test';
import assert from 'node:assert/strict';
import { createGenerationLifecycleService } from '../src/tauri/main/services/ai/generation-lifecycle-service.js';

 test('quiet generation never requests permission or notifies; normal completion still notifies', async () => {
    const notifications = [];
    let permissionRequests = 0;
    const service = createGenerationLifecycleService({
        statusBridge: { supportsProgressUpdates: () => false, handlesCompletion: () => false, reportProgress: () => false },
        estimateTokenCount: text => text.length,
        progressThrottleMs: 4000,
        progressMinCharsDelta: 160,
        notificationService: {
            async preparePermission() { permissionRequests += 1; return 'granted'; },
            async getPermissionState() { return 'granted'; },
            async show(notification) { notifications.push(notification); },
        },
        shouldNotifyCompletion: () => true,
        getNotificationTexts: () => ({
            successTitle: 'Ready', successBody: 'Reply ready',
            failureTitle: 'Failed', failureBody: 'Reply failed',
        }),
        normalizeFailureNotificationBody: (message) => message,
    });
    const quiet = service.createLifecycle({ quiet: true });
    quiet.begin();
    await quiet.finish({ success: true });
    assert.equal(permissionRequests, 0);
    assert.deepEqual(notifications, []);

    const normal = service.createLifecycle({ quiet: false });
    normal.begin();
    await normal.finish({ success: true });
    await normal.finish({ success: true });
    assert.equal(permissionRequests, 1);
    assert.deepEqual(notifications, [{ title: 'Ready', body: 'Reply ready' }]);
});
