'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { EventEmitter, once } = require('node:events');
const vscode = require('vscode');

/**
 * Starts the application the way C# Dev Kit's "Debug project associated with this file" does:
 * a launch of the `dotnet` type that names the project file. The extension must turn it into
 * its own launch, so the breakpoint binds in the application and not in the tool hosting it.
 */
async function run() {
    const project = fs.mkdtempSync(path.join(path.dirname(process.env.ISOPLANE_TEST_PROJECT), 'isoplane-editor-csharp-launch-'));
    fs.cpSync(process.env.ISOPLANE_TEST_PROJECT, project, { recursive: true, filter: source => !['bin', 'obj', '.isoplane'].includes(path.basename(source)) });
    const projectFile = path.join(project, fs.readdirSync(project).find(name => name.endsWith('.csproj')));
    const source = path.join(project, 'Program.cs');
    const line = fs.readFileSync(source, 'utf8').split(/\r?\n/).findIndex(value => value.includes('return answer.ToString();'));
    assert.notEqual(line, -1, 'the prepared application must contain the debugger return statement');
    const breakpoint = new vscode.SourceBreakpoint(new vscode.Location(vscode.Uri.file(source), new vscode.Position(line, 0)));
    const events = new EventEmitter();
    const signal = AbortSignal.timeout(180000);
    let ending = false;
    let reportExit;
    const adapterExit = new Promise(resolve => { reportExit = resolve; });
    const transcript = [];
    const tracker = vscode.debug.registerDebugAdapterTrackerFactory('isoplane', {
        createDebugAdapterTracker() {
            return {
                onWillReceiveMessage(message) { transcript.push("DAP request: " + message.command); },
                onDidSendMessage(message) {
                    if (message.type !== 'event' || message.event !== 'output') transcript.push("DAP: " + JSON.stringify(message));
                    if (message.type === 'event' && message.event === 'output') console.log(message.body.output);
                    if (message.type === 'response' && message.command === 'setBreakpoints' && message.success)
                        for (const reported of message.body.breakpoints) events.emit('breakpoint', reported);
                    if (message.type === 'response' && message.command === 'isoplane/hotReload') events.emit('applied', message);
                    if (message.type !== 'event') return;
                    if (message.event === 'breakpoint') events.emit('breakpoint', message.body.breakpoint);
                    const match = message.event === 'output' && /ready .+ at (http:\/\/\S+)/.exec(message.body.output);
                    if (match) events.emit('ready', match[1]);
                    events.emit(message.event, message.body);
                },
                // VS Code also reports the adapter's closed output here once the session is stopped.
                onError(error) { console.error(error); if (!ending) events.emit('error', error); },
                onExit(code, signal) { transcript.push('Adapter exit: ' + JSON.stringify({ code, signal })); reportExit(); }
            };
        }
    });
    // A customer's launch finds `isoplane` on PATH and uses the launch profile's port; the test names its own.
    const settings = vscode.debug.registerDebugConfigurationProvider('isoplane', {
        resolveDebugConfiguration(_, configuration) { return { ...configuration, cliPath: process.env.ISOPLANE_TEST_CLI, port: 0, watch: false, console: 'internalConsole' }; }
    });
    const ended = new Promise(resolve => {
        const subscription = vscode.debug.onDidTerminateDebugSession(() => { subscription.dispose(); resolve(); });
    });
    // Resolves once the adapter reports a verified breakpoint on the one-based source line.
    const verified = number => new Promise((resolve, reject) => {
        const aborted = () => reject(signal.reason);
        signal.addEventListener('abort', aborted, { once: true });
        events.on('breakpoint', function reported(value) {
            if (!value.verified || value.line !== number) return;
            events.off('breakpoint', reported);
            signal.removeEventListener('abort', aborted);
            resolve();
        });
    });
    vscode.debug.addBreakpoints([breakpoint]);
    let session;
    try {
        const ready = once(events, 'ready', { signal });
        const registered = verified(line + 1);
        assert.equal(await vscode.debug.startDebugging(undefined, {
            type: 'dotnet', request: 'launch', name: 'C# launch of an Isoplane project', projectPath: projectFile
        }), true);
        session = vscode.debug.activeDebugSession;
        assert.equal(session?.type, 'isoplane', 'a C# launch of an Isoplane project must start the Isoplane debugger');
        assert.equal(session.configuration.project, project);
        const [url] = await ready;
        // No request has been sent, so no guest has loaded the code: the built application's symbols verify these.
        await registered;
        await insideALambda(source, session, events, signal, url);
        const added = new vscode.SourceBreakpoint(new vscode.Location(vscode.Uri.file(source), new vscode.Position(line - 1, 0)));
        const addition = verified(line);
        vscode.debug.addBreakpoints([added]);
        await addition;
        vscode.debug.removeBreakpoints([added]);
        const stopping = once(events, 'stopped', { signal });
        const request = fetch(url, { signal });
        request.catch(error => console.error('Paused request: ', error));
        const [stopped] = await stopping;
        const stack = await session.customRequest('stackTrace', { threadId: stopped.threadId });
        assert.equal(stack.stackFrames[0].line, line + 1, 'the breakpoint must bind in the application');
        const refreshed = once(events, 'stopped', { signal });
        const applying = once(events, 'applied', { signal });
        const document = await vscode.workspace.openTextDocument(source);
        const edit = new vscode.WorkspaceEdit();
        edit.replace(document.uri, document.lineAt(line - 1).range, '            int answer = 142;');
        assert.equal(await vscode.workspace.applyEdit(edit), true);
        assert.equal(await document.save(), true);
        const [applied] = await applying;
        assert.equal(applied.success, true, JSON.stringify(applied));
        assert.equal(applied.body.appliedModules, 1);
        assert.deepEqual(applied.body.diagnostics, []);
        assert.equal(applied.body.restartRequired, false);
        const [updated] = await refreshed;
        const frame = (await session.customRequest('stackTrace', { threadId: updated.threadId })).stackFrames[0];
        assert.equal((await session.customRequest('evaluate', { frameId: frame.id, expression: 'answer', context: 'watch' })).result, '42');
        const targets = await session.customRequest('gotoTargets', { source: { path: source }, line });
        assert.equal(targets.targets.length, 1);
        const jumped = once(events, 'stopped', { signal });
        await session.customRequest('goto', { threadId: updated.threadId, targetId: targets.targets[0].id });
        await jumped;
        const reexecuted = once(events, 'stopped', { signal });
        await session.customRequest('continue', { threadId: updated.threadId });
        await reexecuted;
        const shiftedStop = once(events, 'stopped', { signal });
        const shiftedApply = once(events, 'applied', { signal });
        const shiftedBreakpoint = verified(line + 3);
        const shift = new vscode.WorkspaceEdit();
        shift.insert(document.uri, new vscode.Position(0, 0), '\n\n');
        assert.equal(await vscode.workspace.applyEdit(shift), true);
        assert.equal(await document.save(), true);
        const [shiftedReply] = await shiftedApply;
        assert.equal(shiftedReply.success, true, JSON.stringify(shiftedReply));
        assert.deepEqual(shiftedReply.body.diagnostics, []);
        const [shifted] = await shiftedStop;
        await shiftedBreakpoint;
        const shiftedFrame = (await session.customRequest('stackTrace', { threadId: shifted.threadId })).stackFrames[0];
        assert.equal(shiftedFrame.line, line + 3, 'a line-only edit must move the active source location');
        const shiftedLocations = await session.customRequest('breakpointLocations', { source: { path: source }, line: line + 3 });
        assert(shiftedLocations.breakpoints.some(point => point.line === line + 3 && point.column === 13),
            'inline breakpoint suggestions must use the edited source locations');
        const shiftedTargets = await session.customRequest('gotoTargets', { source: { path: source }, line: line + 2 });
        assert.equal(shiftedTargets.targets.length, 1);
        const shiftedJump = once(events, 'stopped', { signal });
        await session.customRequest('goto', { threadId: shifted.threadId, targetId: shiftedTargets.targets[0].id });
        await shiftedJump;
        const shiftedReturn = once(events, 'stopped', { signal });
        await session.customRequest('continue', { threadId: shifted.threadId });
        await shiftedReturn;
        await session.customRequest('continue', { threadId: shifted.threadId });
        assert.equal(await (await request).text(), '142', 'saving and jumping back must execute the edited line in the same invocation');
        console.log('A C# launch of an Isoplane project reached its breakpoint through the Isoplane debugger.');
    } catch (error) {
        console.error(transcript.join('\n'));
        console.error('C# launch scenario: ', error);
        throw error;
    } finally {
        ending = true;
        if (session) {
            await vscode.debug.stopDebugging(session);
            await ended;
            await adapterExit;
        }
        vscode.debug.removeBreakpoints([breakpoint]);
        settings.dispose();
        tracker.dispose();
        fs.rmSync(project, { recursive: true, force: true });
    }
}
/**
 * A breakpoint set with the caret inside a lambda, as "Add Inline Breakpoint" sets one, must become
 * a breakpoint on the lambda's own statement and stop there, not on the statement that contains it.
 */
async function insideALambda(source, session, events, signal, url) {
    const lines = fs.readFileSync(source, 'utf8').split(/\r?\n/);
    const line = lines.findIndex(value => value.includes('value => value > 2'));
    assert.notEqual(line, -1, 'the prepared application must contain the lambda');
    const body = lines[line].indexOf('value > 2');
    const caret = new vscode.Position(line, lines[line].indexOf('> 2'));
    const inline = new vscode.SourceBreakpoint(new vscode.Location(vscode.Uri.file(source), caret));
    const moved = new Promise((resolve, reject) => {
        const aborted = () => reject(signal.reason);
        signal.addEventListener('abort', aborted, { once: true });
        events.on('breakpoint', function reported(value) {
            if (!value.verified || value.line !== line + 1 || value.column !== body + 1) return;
            events.off('breakpoint', reported);
            signal.removeEventListener('abort', aborted);
            resolve();
        });
    });
    vscode.debug.addBreakpoints([inline]);
    try {
        await moved;
        const stopping = once(events, 'stopped', { signal });
        const request = fetch(new URL('/count', url), { signal });
        const [stopped] = await stopping;
        const frame = (await session.customRequest('stackTrace', { threadId: stopped.threadId })).stackFrames[0];
        assert.equal(frame.line, line + 1);
        const value = await session.customRequest('evaluate', { frameId: frame.id, expression: 'value', context: 'watch' });
        assert.equal(value.result, '1', 'the stop must be inside the lambda, at its first element');
        vscode.debug.removeBreakpoints([inline]);
        await session.customRequest('continue', { threadId: stopped.threadId });
        assert.equal(await (await request).text(), '1');
    } finally {
        vscode.debug.removeBreakpoints([inline]);
    }
}
module.exports = { run };
