'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { EventEmitter, once } = require('node:events');
const vscode = require('vscode');

async function run() {
    const extension = vscode.extensions.getExtension('fantail-forest.isoplane-debugger');
    assert.ok(extension);
    await extension.activate();
    const project = fs.mkdtempSync(path.join(os.tmpdir(), 'isoplane-editor-application-'));
    fs.cpSync(process.env.ISOPLANE_TEST_PROJECT, project, { recursive: true, filter: source => !['bin', 'obj', '.isoplane'].includes(path.basename(source)) });
    const source = path.join(project, 'Application.cs');
    const line = fs.readFileSync(source, 'utf8').split(/\r?\n/).findIndex(value => value.includes('return answer.ToString();'));
    assert.notEqual(line, -1, 'the prepared application must contain the debugger return statement');
    const breakpoint = new vscode.SourceBreakpoint(new vscode.Location(vscode.Uri.file(source), new vscode.Position(line, 0)));
    const events = new EventEmitter();
    const signal = AbortSignal.timeout(180000);
    const ready = once(events, 'ready', { signal });
    const tracker = vscode.debug.registerDebugAdapterTrackerFactory('isoplane', {
        createDebugAdapterTracker() {
            return {
                onDidSendMessage(message) {
                    if (message.type !== 'event') return;
                    if (message.event === 'output') {
                        const match = /ready .+ at (http:\/\/\S+)/.exec(message.body.output);
                        if (match) events.emit('ready', match[1]);
                    }
                    events.emit(message.event, message.body);
                },
                onError(error) { events.emit('error', error); }
            };
        }
    });
    vscode.debug.addBreakpoints([breakpoint]);
    let session;
    try {
        assert.equal(await vscode.debug.startDebugging(undefined, {
            type: 'isoplane', request: 'launch', name: 'Launch and edit application',
            project, port: 0, watch: false, cliPath: process.env.ISOPLANE_TEST_CLI
        }), true);
        session = vscode.debug.activeDebugSession;
        assert.ok(session, 'VS Code must own the launched session');
        const [url] = await ready;
        for (const expected of ['84', '42']) {
            const stopping = once(events, 'stopped', { signal });
            const request = fetch(url, { signal });
            const [stopped] = await stopping;
            const stack = await session.customRequest('stackTrace', { threadId: stopped.threadId });
            const frame = stack.stackFrames[0];
            assert.equal(frame.line, line + 1);
            const hover = await session.customRequest('evaluate', { frameId: frame.id, expression: 'answer', context: 'hover' });
            assert.equal(hover.result, '42', 'hover must inspect the current request');
            const watch = await session.customRequest('evaluate', { frameId: frame.id, expression: 'answer + 1', context: 'watch' });
            assert.equal(watch.result, '43');
            if (expected === '84') {
                const scopes = await session.customRequest('scopes', { frameId: frame.id });
                const reference = scopes.scopes.find(scope => scope.name === 'Locals').variablesReference;
                const locals = await session.customRequest('variables', { variablesReference: reference });
                assert.equal(locals.variables.find(value => value.name === 'answer').value, '42');
                const changed = await session.customRequest('setVariable', { variablesReference: reference, name: 'answer', value: 'answer * 2' });
                assert.equal(changed.value, '84');
            }
            await session.customRequest('continue', { threadId: stopped.threadId });
            const response = await request;
            assert.equal(response.status, 200);
            assert.equal(await response.text(), expected);
        }
        await vscode.debug.stopDebugging(session);
    } finally {
        if (session) await vscode.debug.stopDebugging(session);
        vscode.debug.removeBreakpoints([breakpoint]);
        tracker.dispose();
        fs.rmSync(project, { recursive: true, force: true });
    }
}
module.exports = { run };
