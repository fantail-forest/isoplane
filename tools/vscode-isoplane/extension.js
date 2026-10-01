'use strict';
const vscode = require('vscode');

function activate(context) {
    context.subscriptions.push(
        vscode.debug.registerDebugConfigurationProvider('isoplane', {
            resolveDebugConfigurationWithSubstitutedVariables(folder, configuration) {
                if (configuration.request === 'launch') {
                    const project = configuration.project || folder?.uri.fsPath;
                    if (!project) throw new Error('Open an Isoplane project folder or set project in your launch configuration.');
                    return { ...configuration, project };
                }
                const port = Number(configuration.port);
                if (!Number.isInteger(port) || port < 1 || port > 65535) {
                    throw new Error('Enter the debugger port printed by your local Isoplane debug session.');
                }
                return { ...configuration, address: '127.0.0.1', port };
            }
        }),
        vscode.debug.registerDebugAdapterDescriptorFactory('isoplane', {
            createDebugAdapterDescriptor(session) {
                const cli = session.configuration.cliPath || 'isoplane';
                return cli.toLowerCase().endsWith('.dll')
                    ? new vscode.DebugAdapterExecutable('dotnet', [cli, 'debug-adapter'])
                    : new vscode.DebugAdapterExecutable(cli, ['debug-adapter']);
            }
        }),
        vscode.commands.registerCommand('isoplane.detach', async () => {
            const session = vscode.debug.activeDebugSession;
            if (session?.type !== 'isoplane') throw new Error('Select an Isoplane debug session to detach.');
            // Preparation can outlast the editor's timeout for its standard disconnect request.
            await vscode.window.withProgress({ location: vscode.ProgressLocation.Notification, title: 'Preparing Isoplane to run without the debugger' },
                () => session.customRequest('isoplane/prepareDetach'));
            await vscode.commands.executeCommand('workbench.action.debug.disconnect', undefined, { sessionId: session.id });
        }),
        vscode.commands.registerCommand('isoplane.debugPort', () => vscode.window.showInputBox({
            prompt: 'Debugger port printed by the local Isoplane debug session',
            validateInput(value) {
                const port = Number(value);
                return Number.isInteger(port) && port >= 1 && port <= 65535 ? undefined : 'Enter a port from 1 to 65535.';
            }
        }))
    );
}

module.exports = { activate };
