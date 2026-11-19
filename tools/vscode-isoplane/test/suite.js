'use strict';
const assert = require('node:assert/strict');
const net = require('node:net');
const vscode = require('vscode');

async function run() {
    const extension = vscode.extensions.getExtension('Isoplane.isoplane');
    assert.ok(extension, 'VS Code must discover the Isoplane debugger extension');
    await extension.activate();
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
module.exports = { run };
