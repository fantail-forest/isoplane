'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const vscode = require('vscode');

async function run() {
    const extension = vscode.extensions.getExtension('fantail-forest.isoplane-debugger');
    assert.ok(extension, 'VS Code must discover the Isoplane debugger extension');
    await extension.activate();
    csharpLaunches();
    const sockets = new Set();
    const listener = net.createServer(socket => {
        sockets.add(socket);
        socket.on('close', () => sockets.delete(socket));
    });
    await new Promise((resolve, reject) => { listener.once('error', reject); listener.listen(0, '127.0.0.1', resolve); });
    let timer;
    let tracker;
    let cancelledAttachment;
    try {
        const initialized = new Promise((resolve, reject) => {
            timer = setTimeout(() => reject(new Error('VS Code did not initialize the real CLI adapter')), 30000);
            tracker = vscode.debug.registerDebugAdapterTrackerFactory('isoplane', {
                createDebugAdapterTracker() {
                    return {
                        onDidSendMessage(message) {
                            if (message.type === 'response' && message.command === 'initialize') resolve(message);
                            if (message.type === 'response' && message.command === 'attach') cancelledAttachment = message;
                        },
                        onError: reject
                    };
                }
            });
        });
        const starting = vscode.debug.startDebugging(undefined, {
            type: 'isoplane', request: 'attach', name: 'Adapter registration test',
            port: String(listener.address().port), cliPath: process.env.ISOPLANE_TEST_CLI
        });
        const response = await initialized;
        assert.equal(response.success, true, 'the registered executable must speak DAP to VS Code');
        assert.equal(response.body.supportsConditionalBreakpoints, true, 'VS Code must receive the real adapter capabilities');
        await vscode.debug.stopDebugging();
        await assert.rejects(starting, /The local debug session was stopped/, 'cancelling the pending guest handshake must reject attachment');
        assert.equal(cancelledAttachment?.success, false, 'the adapter must complete the cancelled attach request');
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
        assert.deepEqual(isoplaneLaunch(folder, named),
            { type: 'isoplane', request: 'launch', name: 'Debug', project: path.dirname(application), configuration: 'Debug' },
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
