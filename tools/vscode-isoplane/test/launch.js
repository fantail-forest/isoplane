'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { EventEmitter, once } = require('node:events');
const vscode = require('vscode');

async function run() {
    const extension = vscode.extensions.getExtension('fantail-forest.isoplane-debugger');
    assert.ok(extension);
    await extension.activate();
    await require('./csharp').run();
    const project = fs.mkdtempSync(path.join(path.dirname(process.env.ISOPLANE_TEST_PROJECT), 'isoplane-editor-application-'));
    fs.cpSync(process.env.ISOPLANE_TEST_PROJECT, project, { recursive: true, filter: source => !['bin', 'obj', '.isoplane'].includes(path.basename(source)) });
    const source = path.join(project, 'Program.cs');
    const line = fs.readFileSync(source, 'utf8').split(/\r?\n/).findIndex(value => value.includes('return answer.ToString();'));
    assert.notEqual(line, -1, 'the prepared application must contain the debugger return statement');
    const breakpoint = new vscode.SourceBreakpoint(new vscode.Location(vscode.Uri.file(source), new vscode.Position(line, 0)));
    const events = new EventEmitter();
    const signal = AbortSignal.timeout(180000);
    const ready = once(events, 'ready', { signal });
    let reportExit;
    const adapterExit = new Promise(resolve => { reportExit = resolve; });
    const tracker = vscode.debug.registerDebugAdapterTrackerFactory('isoplane', {
        createDebugAdapterTracker() {
            let disconnected = false;
            return {
                onWillReceiveMessage(message) {
                    if (message.command === 'disconnect') {
                        console.log('Editor disconnect: ' + JSON.stringify(message));
                        events.emit('disconnect', message.arguments);
                    }
                },
                onDidSendMessage(message) {
                    if (message.type === 'response' && message.command === 'disconnect') {
                        disconnected = message.success;
                        console.log('Adapter disconnect: ' + JSON.stringify(message));
                    }
                    if (message.type !== 'event') return;
                    if (message.event === 'output') {
                        const match = /ready .+ at (http:\/\/\S+)/.exec(message.body.output);
                        if (match) events.emit('ready', match[1]);
                    }
                    events.emit(message.event, message.body);
                },
                onError(error) {
                    // VS Code also reports stdout closure here after an acknowledged disconnect.
                    if (!disconnected) events.emit('error', error);
                    else console.log('Adapter transport closed after disconnect: ' + error.message);
                },
                onExit(code, signal) { reportExit(); events.emit('adapter-exit', { code, signal }); }
            };
        }
    });
    vscode.debug.addBreakpoints([breakpoint]);
    let stage = 'launch';
    let session;
    let terminal;
    const terminated = vscode.debug.onDidTerminateDebugSession(value => events.emit('session-ended', value));
    const terminals = vscode.window.onDidOpenTerminal(value => {
        if (value.name === 'Isoplane') terminal = value;
    });
    try {
        assert.equal(await vscode.debug.startDebugging(undefined, {
            type: 'isoplane', request: 'launch', name: 'Launch and edit application',
            project, port: 0, watch: false, cliPath: process.env.ISOPLANE_TEST_CLI
        }), true);
        session = vscode.debug.activeDebugSession;
        assert.ok(session, 'VS Code must own the launched session');
        const [url] = await ready;
        for (const expected of ['84', '42']) {
            stage = 'request returning ' + expected;
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
        stage = 'pause before disconnect';
        assert.ok(terminal, 'the editor must own an Isoplane terminal');
        const stopping = once(events, 'stopped', { signal });
        const activeRequest = fetch(url, { signal });
        await stopping;
        const disconnecting = once(events, 'disconnect', { signal });
        const ending = once(events, 'session-ended', { signal });
        const exiting = once(events, 'adapter-exit', { signal });
        stage = 'editor Disconnect command';
        await vscode.commands.executeCommand('isoplane.detach');
        stage = 'disconnect request';
        const [disconnect] = await disconnecting;
        assert.equal(disconnect.terminateDebuggee, false, 'the editor Disconnect command must preserve the application');
        stage = 'debug session termination';
        await ending;
        const [exit] = await exiting;
        assert.equal(exit.code, 0, 'acknowledged detach must end the adapter cleanly');
        session = undefined;
        assert.equal(await (await activeRequest).text(), '42', 'detach must resume the active request');
        stage = 'request after detach';
        let detachedResponse;
        do {
            detachedResponse = await fetch(url, { signal });
            if (detachedResponse.status === 503) await detachedResponse.text();
        } while (detachedResponse.status === 503);
        assert.equal(detachedResponse.status, 200);
        assert.equal(await detachedResponse.text(), '42', 'the terminal application must serve after detach');
        stage = 'Ctrl+C terminal shutdown';
        const sessions = path.join(project, '.isoplane', 'debug');
        await new Promise((resolve, reject) => {
            const watcher = fs.watch(sessions, check);
            const aborted = () => finish(signal.reason);
            function finish(error) {
                watcher.close();
                signal.removeEventListener('abort', aborted);
                if (error) reject(error); else resolve();
            }
            function check() {
                if (fs.readdirSync(sessions).length === 0) finish();
            }
            watcher.on('error', finish);
            signal.addEventListener('abort', aborted, { once: true });
            terminal.sendText('\u0003', false);
            check();
        });
        assert.deepEqual(fs.readdirSync(sessions), [], 'Ctrl+C must release the detached session artifacts');
        console.log('Isoplane editor launch, value editing, detach and terminal shutdown passed.');
    } catch (error) {
        console.error(error);
        throw new Error('Editor debugger scenario failed at ' + stage, { cause: error });
    } finally {
        if (session) {
            await vscode.debug.stopDebugging(session);
            await adapterExit;
        }
        if (terminal) terminal.dispose();
        terminals.dispose();
        terminated.dispose();
        vscode.debug.removeBreakpoints([breakpoint]);
        tracker.dispose();
        fs.rmSync(project, { recursive: true, force: true });
    }
}
module.exports = { run };
