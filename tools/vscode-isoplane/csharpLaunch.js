'use strict';
const path = require('node:path');
const { isIsoplaneProject } = require('./projectSdk');

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
async function isoplaneLaunch(folder, configuration) {
    if (configuration.request !== 'launch' || configuration.noDebug) return undefined;
    const expand = value => folder ? value.replaceAll('${workspaceFolder}', folder.uri.fsPath) : value;
    // C# Dev Kit's own buttons name the launch after the project file's path.
    const named = configuration.name && configuration.name !== configuration.projectPath;
    const launch = { type: 'isoplane', request: 'launch', name: named ? configuration.name : 'Debug local Isoplane' };
    if (configuration.type === 'dotnet' && typeof configuration.projectPath === 'string') {
        const file = expand(configuration.projectPath);
        return await isIsoplaneProject(file) ? { ...launch, project: path.dirname(file), configuration: 'Debug' } : undefined;
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

module.exports = { isoplaneLaunch };
