'use strict';
const path = require('node:path');

function words(value) {
    if (Array.isArray(value)) return value.map(String);
    return typeof value === 'string' ? (value.match(/"[^"]*"|\S+/g) || []).map(word => word.replace(/^"|"$/g, '')) : [];
}

/**
 * The Isoplane launch that a C# launch of an Isoplane project stands for, or undefined when the
 * configuration is anything else. The C# debugger would attach to the `isoplane` tool, which only
 * hosts the application: its code runs in the sandbox, where this extension's adapter reaches it.
 * A C# launch names the project (`dotnet`) or the project's run command (`coreclr`).
 * `isIsoplaneProject` answers, for a project file's absolute path, whether it is an Isoplane project.
 */
async function isoplaneLaunch(folder, configuration, isIsoplaneProject) {
    if (configuration.request !== 'launch' || configuration.noDebug) return undefined;
    const expand = value => folder ? value.replaceAll('${workspaceFolder}', folder.uri.fsPath) : value;
    // C# Dev Kit's own buttons name the launch after the project file's path.
    const named = configuration.name && configuration.name !== configuration.projectPath;
    const launch = { type: 'isoplane', request: 'launch', name: named ? configuration.name : 'Debug local Isoplane' };
    if (configuration.type === 'dotnet' && typeof configuration.projectPath === 'string') {
        const projectPath = expand(configuration.projectPath);
        // A variable the editor has yet to substitute leaves the project unnamed until its later hook.
        // Resolving the path first would cancel the variable against a `..` that follows it.
        if (projectPath.includes('${')) return undefined;
        // A relative path is relative to the workspace folder, as the C# debugger reads it.
        const file = path.resolve(folder?.uri.fsPath ?? '', projectPath);
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

/**
 * The debug configuration provider for C# launches, which replaces a launch of an Isoplane project
 * with its Isoplane launch and returns any other unchanged. The editor calls the first hook, then
 * substitutes variables, then calls the second with a copy; a project the first hook classified
 * is not classified again by the second. Each first hook forgets earlier classifications, so a
 * launch never reads an answer from before it started.
 */
function csharpLaunchProvider(isIsoplaneProject) {
    let classified = new Map();
    const classify = file => {
        if (!classified.has(file)) classified.set(file, isIsoplaneProject(file));
        return classified.get(file);
    };
    return {
        async resolveDebugConfiguration(folder, configuration) {
            classified = new Map();
            return await isoplaneLaunch(folder, configuration, classify) || configuration;
        },
        async resolveDebugConfigurationWithSubstitutedVariables(folder, configuration) {
            return await isoplaneLaunch(folder, configuration, classify) || configuration;
        }
    };
}

module.exports = { csharpLaunchProvider };
