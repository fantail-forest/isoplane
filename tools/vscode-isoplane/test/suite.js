'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const vscode = require('vscode');

async function run() {
    const extension = vscode.extensions.getExtension('Isoplane.isoplane');
    assert.ok(extension, 'VS Code must discover the Isoplane debugger extension');
    await extension.activate();
    csharpLaunches();
    const sockets = new Set();
    let connected;
    const connecting = new Promise(resolve => { connected = resolve; });
    const listener = net.createServer(socket => {
        sockets.add(socket);
        socket.on('close', () => sockets.delete(socket));
        connected();
    });
    await new Promise((resolve, reject) => { listener.once('error', reject); listener.listen(0, '127.0.0.1', resolve); });
    let timer;
    let tracker;
    let attached;
    const attachment = new Promise(resolve => { attached = resolve; });
    try {
        const deadline = new Promise((_, reject) => {
            timer = setTimeout(() => reject(new Error('The real CLI adapter did not complete the attachment cancellation scenario')), 30000);
        });
        const initialized = new Promise((resolve, reject) => {
            tracker = vscode.debug.registerDebugAdapterTrackerFactory('isoplane', {
                createDebugAdapterTracker() {
                    return {
                        onDidSendMessage(message) {
                            if (message.type === 'response' && message.command === 'initialize') resolve(message);
                            if (message.type === 'response' && message.command === 'attach') attached(message);
                        },
                        onError: reject
                    };
                }
            });
        });
        const starting = vscode.debug.startDebugging(undefined, {
            type: 'isoplane', request: 'attach', name: 'Adapter registration test',
            port: String(listener.address().port), cliPath: process.env.ISOPLANE_TEST_CLI
        }).catch(error => assert.match(error.message, /The local debug session was stopped/));
        const response = await Promise.race([initialized, deadline]);
        assert.equal(response.success, true, 'the registered executable must speak DAP to VS Code');
        assert.equal(response.body.supportsConditionalBreakpoints, true, 'VS Code must receive the real adapter capabilities');
        await Promise.race([connecting, deadline]);
        await vscode.debug.stopDebugging();
        const cancelledAttachment = await Promise.race([attachment, deadline]);
        assert.equal(cancelledAttachment.success, false, 'the adapter must complete the cancelled attach request');
        assert.match(cancelledAttachment.message, /The local debug session was stopped/);
        await starting;
    } finally {
        clearTimeout(timer);
        tracker?.dispose();
        for (const socket of sockets) socket.destroy();
        await new Promise(resolve => listener.close(resolve));
    }
}
/** Which C# launches the extension takes over, and which it leaves to the C# debugger. */
function csharpLaunches() {
    const { isoplaneLaunch } = require('../extension.js');
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'isoplane-editor-projects-'));
    try {
        const application = path.join(directory, 'app', 'App.csproj');
        const library = path.join(directory, 'library', 'Library.csproj');
        for (const file of [application, library]) fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(application, '<!-- A leading comment. -->\n<Project Sdk="Isoplane.AspNetCore/1.2.3">\n</Project>\n');
        fs.writeFileSync(library, '<Project Sdk="Microsoft.NET.Sdk">\n</Project>\n');
        const folder = { uri: vscode.Uri.file(directory) };
        const named = { type: 'dotnet', request: 'launch', name: 'Debug', projectPath: '${workspaceFolder}/app/App.csproj' };
        const launch = isoplaneLaunch(folder, named);
        assert.deepEqual({ ...launch, project: fs.realpathSync.native(launch.project) },
            { type: 'isoplane', request: 'launch', name: 'Debug', project: fs.realpathSync.native(path.dirname(application)), configuration: 'Debug' },
            'a launch naming an Isoplane project file must become an Isoplane launch of its directory');
        assert.equal(isoplaneLaunch(folder, { ...named, projectPath: library }), undefined, 'any other project keeps the C# debugger');
        assert.equal(isoplaneLaunch(folder, { ...named, noDebug: true }), undefined, 'running without debugging keeps the project run command');
        assert.equal(isoplaneLaunch(folder, { ...named, request: 'attach' }), undefined);
        const command = { type: 'coreclr', request: 'launch', name: 'Debug', program: 'isoplane' };
        assert.deepEqual(isoplaneLaunch(folder, { ...command, args: 'dev --project "/my projects/app" --configuration "Release" --no-watch --no-build' }),
            { type: 'isoplane', request: 'launch', name: 'Debug', project: '/my projects/app', configuration: 'Release' },
            'a launch of the project run command must become an Isoplane launch of the project it names');
        assert.equal(isoplaneLaunch(folder, { ...command, program: '/usr/bin/dotnet', args: ['/tools/Isoplane.Cli.dll', 'dev', '--project', '/app'] })?.project, '/app');
        assert.equal(isoplaneLaunch(folder, { ...command, program: '/app/bin/App.dll', args: [] }), undefined, 'any other program keeps the C# debugger');
        assert.equal(isoplaneLaunch(folder, { ...command, args: ['deploy', '--project', '/app'] }), undefined);
    } finally {
        fs.rmSync(directory, { recursive: true, force: true });
    }
}
module.exports = { run };
