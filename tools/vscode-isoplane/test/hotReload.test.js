'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

function registeredHotReload() {
    let factory;
    let saved;
    let command;
    const requests = [];
    const diagnostics = [];
    const session = {
        id: 'hot-reload-test',
        customRequest(name, request) {
            assert.equal(name, 'isoplane/hotReload');
            return new Promise((resolve, reject) => requests.push({
                sources: Array.from(request.sources, source => ({ ...source })), resolve, reject
            }));
        }
    };
    const vscode = {
        ProgressLocation: { Window: 10 },
        window: {
            createOutputChannel: () => ({ appendLine: line => diagnostics.push(line), show() {} }),
            withProgress: (_, action) => action()
        },
        debug: {
            activeDebugSession: session,
            registerDebugAdapterTrackerFactory: (_, value) => { factory = value; }
        },
        workspace: {
            onDidSaveTextDocument: callback => { saved = callback; },
            saveAll: async () => true
        },
        commands: {
            registerCommand(name, callback) {
                assert.equal(name, 'isoplane.applyCodeChanges');
                command = callback;
            }
        }
    };
    const module = { exports: {} };
    const filename = path.resolve(__dirname, '../hotReload.js');
    vm.runInNewContext(fs.readFileSync(filename, 'utf8'), {
        module, process, require: name => name === 'vscode' ? vscode : require(name)
    }, { filename });
    module.exports.register({ subscriptions: [] });
    const tracker = factory.createDebugAdapterTracker(session);
    return {
        requests, diagnostics, command,
        ready(documents) {
            tracker.onDidSendMessage({ type: 'event', event: 'isoplane/hotReloadReady', body: { documents } });
        },
        save(file, text) {
            saved({ uri: { scheme: 'file', fsPath: file }, getText: () => text });
        },
        exit: () => tracker.onExit()
    };
}

const first = path.resolve('First.cs');
const second = path.resolve('Second.cs');
const applied = { restartRequired: false, diagnostics: [] };
const failed = { restartRequired: false, diagnostics: ['The edit did not compile.'] };
const flush = () => new Promise(setImmediate);

for (const failure of ['diagnostic', 'request error']) {
    test(`a failed apply with ${failure} waits for an explicit retry when there are no new saves`, async () => {
        const reload = registeredHotReload();
        reload.ready([first]);
        reload.save(first, 'invalid edit');
        if (failure === 'diagnostic') reload.requests[0].resolve(failed);
        else reload.requests[0].reject(new Error('The debug transport failed.'));
        await flush();
        assert.equal(reload.requests.length, 1, 'a failed batch must not retry itself');
        assert.equal(reload.diagnostics.length, 1, 'the failure must be reported');

        const retrying = reload.command();
        await flush();
        assert.equal(reload.requests.length, 2, 'the command must still retry the retained edit');
        assert.deepEqual(reload.requests[1].sources, [{ path: first, text: 'invalid edit' }]);
        reload.requests[1].resolve(applied);
        await retrying;
        assert.equal(reload.requests.length, 2);
    });
}

test('a newer save replaces the failed text and applies without another trigger', async () => {
    const reload = registeredHotReload();
    reload.ready([first]);
    reload.save(first, 'invalid edit');
    reload.save(first, 'corrected edit');
    assert.equal(reload.requests.length, 1, 'applies must remain serialized');
    reload.requests[0].resolve(failed);
    await flush();
    assert.equal(reload.requests.length, 2, 'the save queued during the failure must be applied');
    assert.deepEqual(reload.requests[1].sources, [{ path: first, text: 'corrected edit' }]);
    reload.requests[1].resolve(applied);
    await flush();
    assert.equal(reload.requests.length, 2);
});

test('a save to another file includes the failed edit once and waits after another failure', async () => {
    const reload = registeredHotReload();
    reload.ready([first, second]);
    reload.save(first, 'first edit');
    reload.save(second, 'second edit');
    reload.requests[0].resolve(failed);
    await flush();
    assert.equal(reload.requests.length, 2);
    assert.deepEqual(reload.requests[1].sources, [
        { path: second, text: 'second edit' }, { path: first, text: 'first edit' }
    ]);
    reload.requests[1].resolve(failed);
    await flush();
    assert.equal(reload.requests.length, 2, 'merging a failed batch must not count as another save');
});

for (const outcome of ['success', 'failure']) {
    test(`a new ready generation drains its saves after an older ${outcome} without restoring old edits`, async () => {
        const reload = registeredHotReload();
        reload.ready([first]);
        reload.save(first, 'old generation edit');
        reload.ready([second]);
        reload.save(second, 'new generation edit');
        assert.equal(reload.requests.length, 1);
        reload.requests[0].resolve(outcome === 'success' ? applied : failed);
        await flush();
        assert.equal(reload.requests.length, 2, 'the old request must release the new generation queue');
        assert.deepEqual(reload.requests[1].sources, [{ path: second, text: 'new generation edit' }]);
        reload.requests[1].resolve(applied);
        await flush();
        assert.equal(reload.requests.length, 2);
    });
}

test('an exited session does not drain edits when its in-flight apply completes', async () => {
    const reload = registeredHotReload();
    reload.ready([first, second]);
    reload.save(first, 'in-flight edit');
    reload.save(second, 'queued edit');
    reload.exit();
    reload.requests[0].resolve(failed);
    await flush();
    reload.save(first, 'edit after exit');
    await flush();
    assert.equal(reload.requests.length, 1, 'exit must prevent further requests for the session');
    await assert.rejects(reload.command(), /Select an Isoplane source debugging session/);
});
