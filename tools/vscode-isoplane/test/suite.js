'use strict';
const assert = require('node:assert/strict');
const net = require('node:net');
const vscode = require('vscode');

async function run() {
    const extension = vscode.extensions.getExtension('fantail-forest.isoplane-debugger');
    assert.ok(extension, 'VS Code must discover the Isoplane debugger extension');
    await extension.activate();
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
module.exports = { run };
