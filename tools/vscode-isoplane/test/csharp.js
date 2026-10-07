'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { EventEmitter, once } = require('node:events');
const vscode = require('vscode');

/**
 * Starts the application the way C# Dev Kit's "Debug project associated with this file" does:
 * a launch of the `dotnet` type that names the project file. The extension must turn it into
 * its own launch, so the breakpoint binds in the application and not in the tool hosting it.
 */
async function run() {
    const project = fs.mkdtempSync(path.join(os.tmpdir(), 'isoplane-editor-csharp-launch-'));
    fs.cpSync(process.env.ISOPLANE_TEST_PROJECT, project, { recursive: true, filter: source => !['bin', 'obj', '.isoplane'].includes(path.basename(source)) });
    const projectFile = path.join(project, fs.readdirSync(project).find(name => name.endsWith('.csproj')));
    const source = path.join(project, 'Program.cs');
    const line = fs.readFileSync(source, 'utf8').split(/\r?\n/).findIndex(value => value.includes('return answer.ToString();'));
    assert.notEqual(line, -1, 'the prepared application must contain the debugger return statement');
    const breakpoint = new vscode.SourceBreakpoint(new vscode.Location(vscode.Uri.file(source), new vscode.Position(line, 0)));
    const events = new EventEmitter();
    const signal = AbortSignal.timeout(180000);
    let ending = false;
    const tracker = vscode.debug.registerDebugAdapterTrackerFactory('isoplane', {
        createDebugAdapterTracker() {
            return {
                onDidSendMessage(message) {
                    if (message.type !== 'event') return;
                    const match = message.event === 'output' && /ready .+ at (http:\/\/\S+)/.exec(message.body.output);
                    if (match) events.emit('ready', match[1]);
                    events.emit(message.event, message.body);
                },
                // VS Code also reports the adapter's closed output here once the session is stopped.
                onError(error) { if (!ending) events.emit('error', error); }
            };
        }
    });
    // A customer's launch finds `isoplane` on PATH and uses the launch profile's port; the test names its own.
    const settings = vscode.debug.registerDebugConfigurationProvider('isoplane', {
        resolveDebugConfiguration(_, configuration) { return { ...configuration, cliPath: process.env.ISOPLANE_TEST_CLI, port: 0, watch: false }; }
    });
    const ended = new Promise(resolve => {
        const subscription = vscode.debug.onDidTerminateDebugSession(() => { subscription.dispose(); resolve(); });
    });
    vscode.debug.addBreakpoints([breakpoint]);
    let session;
    try {
        const ready = once(events, 'ready', { signal });
        assert.equal(await vscode.debug.startDebugging(undefined, {
            type: 'dotnet', request: 'launch', name: 'C# launch of an Isoplane project', projectPath: projectFile
        }), true);
        session = vscode.debug.activeDebugSession;
        assert.equal(session?.type, 'isoplane', 'a C# launch of an Isoplane project must start the Isoplane debugger');
        assert.equal(session.configuration.project, project);
        const [url] = await ready;
        const stopping = once(events, 'stopped', { signal });
        const request = fetch(url, { signal });
        const [stopped] = await stopping;
        const stack = await session.customRequest('stackTrace', { threadId: stopped.threadId });
        assert.equal(stack.stackFrames[0].line, line + 1, 'the breakpoint must bind in the application');
        await session.customRequest('continue', { threadId: stopped.threadId });
        assert.equal(await (await request).text(), '42');
        console.log('A C# launch of an Isoplane project reached its breakpoint through the Isoplane debugger.');
    } finally {
        ending = true;
        if (session) {
            await vscode.debug.stopDebugging(session);
            await ended;
        }
        vscode.debug.removeBreakpoints([breakpoint]);
        settings.dispose();
        tracker.dispose();
        fs.rmSync(project, { recursive: true, force: true });
    }
}
module.exports = { run };
