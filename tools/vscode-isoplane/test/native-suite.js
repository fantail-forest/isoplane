'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vscode = require('vscode');

async function run() {
    const project = process.env.ISOPLANE_TEST_NATIVE_PROJECT;
    assert.ok(project, 'the native editor test requires its fixture project');
    const source = path.join(project, 'Application.cs');
    const line = fs.readFileSync(source, 'utf8').split('\n').findIndex(value => value.includes('answer *= 2;'));
    assert.ok(line >= 0, 'the native editor fixture must contain the breakpoint statement');
    const breakpoint = new vscode.SourceBreakpoint(new vscode.Location(vscode.Uri.file(source), new vscode.Position(line, 0)));
    vscode.debug.addBreakpoints([breakpoint]);
    const messages = [];
    let notify;
    const tracker = vscode.debug.registerDebugAdapterTrackerFactory('isoplane', {
        createDebugAdapterTracker() {
            return { onDidSendMessage(message) { messages.push(message); notify?.(); } };
        }
    });
    async function until(predicate) {
        while (true) {
            const found = messages.findIndex(predicate);
            if (found >= 0) return messages.splice(found, 1)[0];
            await new Promise(resolve => { notify = resolve; });
        }
    }
    let timer;
    let http;
    let terminationListener;
    const terminated = new Promise(resolve => {
        terminationListener = vscode.debug.onDidTerminateDebugSession(session => {
            if (session.name === 'NativeAOT editor acceptance') resolve();
        });
    });
    async function exercise() {
        const started = await vscode.debug.startDebugging(undefined, {
            type: 'isoplane', request: 'launch', name: 'NativeAOT editor acceptance', project,
            configuration: 'IsoplaneNativeAotDebug', httpPort: 0, cliPath: process.env.ISOPLANE_TEST_CLI
        });
        assert.ok(started, 'VS Code must launch the actual NativeAOT debugger');
        const ready = await until(message => message.event === 'output' && /ready .* at http:\/\/127\.0\.0\.1:\d+/.test(message.body.output));
        const url = ready.body.output.match(/http:\/\/127\.0\.0\.1:\d+/)[0];
        http = fetch(url).then(async response => [response.status, await response.text()]);
        const stopped = await until(message => message.event === 'stopped');
        const session = vscode.debug.activeDebugSession;
        assert.ok(session, 'the native debug session must remain active at the source breakpoint');
        const stack = await session.customRequest('stackTrace', { threadId: stopped.body.threadId });
        assert.equal(stack.stackFrames[0].source.path, source, 'VS Code must stop in the guest C# source');
        assert.equal(stack.stackFrames[0].line, line + 1, 'VS Code must honor the editor source breakpoint');
        const value = await session.customRequest('evaluate', { expression: 'answer', frameId: stack.stackFrames[0].id, context: 'watch' });
        assert.equal(value.result, '21', 'the editor must inspect the real NativeAOT local');
        await session.customRequest('continue', { threadId: stopped.body.threadId });
        assert.deepEqual(await http, [200, '42'], 'continuing in the editor must finish the native guest request');
        await terminated;
    }
    try {
        await Promise.race([
            exercise(),
            new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Native editor acceptance timed out: ' + JSON.stringify(messages))), 600000); })
        ]);
    } catch (error) {
        console.error('Native editor acceptance failed:', error, JSON.stringify(messages.slice(-30)));
        throw error;
    } finally {
        clearTimeout(timer);
        if (vscode.debug.activeDebugSession) await vscode.debug.stopDebugging();
        terminationListener.dispose();
        tracker.dispose();
        vscode.debug.removeBreakpoints([breakpoint]);
        if (http) await http.catch(error => console.log('HTTP closed during debugger cleanup: ' + error.message));
    }
}
module.exports = { run };
