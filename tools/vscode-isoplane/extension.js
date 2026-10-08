'use strict';
const fs = require('node:fs');
const path = require('node:path');
const vscode = require('vscode');

const isoplaneSdk = /<Project\s[^>]*\bSdk\s*=\s*"Isoplane\.(Sdk|AspNetCore)[/"]/;

function isoplaneProjectFile(file) {
    try { return file.toLowerCase().endsWith('.csproj') && isoplaneSdk.test(fs.readFileSync(file, 'utf8')); }
    catch { return false; }
}

function words(value) {
    if (Array.isArray(value)) return value.map(String);
    return typeof value === 'string' ? (value.match(/"[^"]*"|\S+/g) || []).map(word => word.replace(/^"|"$/g, '')) : [];
}

/**
 * The Isoplane launch that a C# launch of an Isoplane project stands for, or undefined when the
 * configuration is anything else. The C# debugger would attach to the `isoplane` tool, which only
 * hosts the application: its code runs in the sandbox, where this extension's adapter reaches it.
 * A C# launch names the project (`dotnet`) or the project's run command (`coreclr`).
 */
function isoplaneLaunch(folder, configuration) {
    if (configuration.request !== 'launch' || configuration.noDebug) return undefined;
    const expand = value => folder ? value.replaceAll('${workspaceFolder}', folder.uri.fsPath) : value;
    // C# Dev Kit's own buttons name the launch after the project file's path.
    const named = configuration.name && configuration.name !== configuration.projectPath;
    const launch = { type: 'isoplane', request: 'launch', name: named ? configuration.name : 'Debug local Isoplane' };
    if (configuration.type === 'dotnet' && typeof configuration.projectPath === 'string') {
        const file = expand(configuration.projectPath);
        return isoplaneProjectFile(file) ? { ...launch, project: path.dirname(file), configuration: 'Debug' } : undefined;
    }
    if (configuration.type !== 'coreclr' || typeof configuration.program !== 'string') return undefined;
    const command = [path.basename(configuration.program).toLowerCase(), ...words(configuration.args)];
    const tool = command[0] === 'isoplane' || command[0] === 'isoplane.exe' ? 1
        : command[1]?.toLowerCase().endsWith('isoplane.cli.dll') ? 2 : 0;
    const project = command.indexOf('--project');
    if (!tool || command[tool] !== 'dev' || project < 0 || !command[project + 1]) return undefined;
    const selected = command.indexOf('--configuration');
    return { ...launch, project: expand(command[project + 1]), configuration: selected < 0 ? 'Debug' : command[selected + 1] };
}

const csharpLaunch = {
    resolveDebugConfiguration(folder, configuration) { return isoplaneLaunch(folder, configuration) || configuration; },
    resolveDebugConfigurationWithSubstitutedVariables(folder, configuration) { return isoplaneLaunch(folder, configuration) || configuration; }
};

function activate(context) {
    require('./hotReload').register(context);
    context.subscriptions.push(
        vscode.debug.registerDebugConfigurationProvider('dotnet', csharpLaunch),
        vscode.debug.registerDebugConfigurationProvider('coreclr', csharpLaunch),
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

module.exports = { activate, isoplaneLaunch };
