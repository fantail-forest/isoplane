'use strict';
const vscode = require('vscode');
const { adapterCommand, adapterProblem } = require('./adapterCheck');
const { csharpLaunchProvider } = require('./csharpLaunch');
const { isIsoplaneProject } = require('./projectSdk');

const csharpLaunch = csharpLaunchProvider(isIsoplaneProject);

function activate(context) {
    require('./hotReload').register(context);
    context.subscriptions.push(
        vscode.debug.registerDebugConfigurationProvider('dotnet', csharpLaunch),
        vscode.debug.registerDebugConfigurationProvider('coreclr', csharpLaunch),
        vscode.debug.registerDebugConfigurationProvider('isoplane', {
            async resolveDebugConfigurationWithSubstitutedVariables(folder, configuration) {
                const problem = await adapterProblem(adapterCommand(configuration.cliPath));
                if (problem) {
                    vscode.window.showErrorMessage(problem);
                    return undefined;
                }
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
                return new vscode.DebugAdapterExecutable(...adapterCommand(session.configuration.cliPath));
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
