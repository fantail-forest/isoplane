'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const test = require('node:test');
const { csharpLaunchProvider } = require('../csharpLaunch.js');
const { isIsoplaneProject } = require('../projectSdk.js');
const { directoryOf, isoplaneProps } = require('./projectFiles.js');

/** A provider that counts a fixed set of project files as Isoplane projects, and the files it was asked about. */
function providerOf(...isoplaneProjects) {
    const asked = [];
    const provider = csharpLaunchProvider(async file => { asked.push(file); return isoplaneProjects.includes(file); });
    return { provider, asked };
}

/**
 * What the editor launches for `configuration`: the first hook's answer when that is another
 * debugger's launch, and otherwise the second hook's answer for the editor's copy.
 */
async function launched(provider, folder, configuration) {
    const first = await provider.resolveDebugConfiguration(folder, configuration);
    if (first !== configuration) return first;
    return provider.resolveDebugConfigurationWithSubstitutedVariables(folder, { ...configuration });
}

test('a C# launch of an Isoplane project file becomes an Isoplane launch of its directory', async t => {
    const directory = directoryOf(t, {
        'Common.props': isoplaneProps,
        'app/App.csproj': '<Project><Import Project="../Common.props" /></Project>',
        'app/-Demo.csproj': '<Project><Import Project="../Common.props" /></Project>',
        'library/Library.csproj': '<Project />'
    });
    const folder = { uri: { fsPath: directory } };
    const provider = csharpLaunchProvider(isIsoplaneProject);
    const application = { type: 'isoplane', request: 'launch', name: 'Debug', project: path.join(directory, 'app'), configuration: 'Debug' };
    const csharp = { type: 'dotnet', request: 'launch', name: 'Debug', projectPath: path.join(directory, 'app', 'App.csproj') };
    const library = { ...csharp, projectPath: path.join(directory, 'library', 'Library.csproj') };

    assert.deepEqual(await launched(provider, folder, csharp), application);
    assert.deepEqual(await launched(provider, folder, { ...csharp, projectPath: path.join(directory, 'app', '-Demo.csproj') }), application,
        'a project file whose name starts with a hyphen is still a project');
    assert.deepEqual(await launched(provider, folder, library), library, 'any other project keeps the C# debugger');
});

test('a C# launch names its project relative to the workspace folder', async () => {
    const workspace = path.resolve('workspace');
    const file = path.join(workspace, 'app', 'App.csproj');
    const folder = { uri: { fsPath: workspace } };
    const { provider, asked } = providerOf(file);
    const application = { type: 'isoplane', request: 'launch', name: 'Debug', project: path.join(workspace, 'app'), configuration: 'Debug' };
    const csharp = { type: 'dotnet', request: 'launch', name: 'Debug', projectPath: '${workspaceFolder}/app/App.csproj' };

    assert.deepEqual(await launched(provider, folder, csharp), application);
    assert.deepEqual(await launched(provider, folder, { ...csharp, projectPath: 'app/App.csproj' }), application);
    assert.deepEqual(asked, [file, file]);
});

test('running without debugging and attaching keep the C# configuration', async () => {
    const file = path.resolve('app', 'App.csproj');
    const { provider, asked } = providerOf(file);
    const csharp = { type: 'dotnet', request: 'launch', name: 'Debug', projectPath: file };
    const withoutDebugging = { ...csharp, noDebug: true };
    const attach = { ...csharp, request: 'attach' };

    assert.equal((await launched(provider, undefined, csharp)).type, 'isoplane');
    assert.equal(await provider.resolveDebugConfiguration(undefined, withoutDebugging), withoutDebugging);
    assert.deepEqual(await launched(provider, undefined, withoutDebugging), withoutDebugging);
    assert.equal(await provider.resolveDebugConfiguration(undefined, attach), attach);
    assert.deepEqual(await launched(provider, undefined, attach), attach);
    assert.deepEqual(asked, [file]);
});

test('a C# launch of the project run command becomes an Isoplane launch of the project it names', async () => {
    const { provider, asked } = providerOf();
    const command = { type: 'coreclr', request: 'launch', name: 'Debug', program: 'isoplane' };
    const otherProgram = { ...command, program: '/app/bin/App.dll', args: [] };
    const otherSubcommand = { ...command, args: ['deploy', '--project', '/app'] };

    assert.deepEqual(await launched(provider, undefined, { ...command, args: 'dev --project "/my projects/app" --configuration "Release" --no-watch --no-build' }),
        { type: 'isoplane', request: 'launch', name: 'Debug', project: '/my projects/app', configuration: 'Release' });
    assert.deepEqual(await launched(provider, undefined, { ...command, program: '/usr/bin/dotnet', args: ['/tools/Isoplane.Cli.dll', 'dev', '--project', '/app'] }),
        { type: 'isoplane', request: 'launch', name: 'Debug', project: '/app', configuration: 'Debug' });
    assert.deepEqual(await launched(provider, undefined, otherProgram), otherProgram, 'any other program keeps the C# debugger');
    assert.deepEqual(await launched(provider, undefined, otherSubcommand), otherSubcommand);
    assert.deepEqual(asked, [], 'the run command names an Isoplane project by itself');
});

test('a launch classifies its project once across both hooks, and the next launch classifies it again', async () => {
    const file = path.resolve('app', 'App.csproj');
    const answers = [false, true];
    const asked = [];
    const provider = csharpLaunchProvider(async project => { asked.push(project); return answers[asked.length - 1]; });
    const csharp = { type: 'dotnet', request: 'launch', name: 'Debug', projectPath: file };

    assert.equal(await provider.resolveDebugConfiguration(undefined, csharp), csharp);
    assert.deepEqual(await provider.resolveDebugConfigurationWithSubstitutedVariables(undefined, { ...csharp }), csharp);
    assert.deepEqual(asked, [file]);

    assert.equal((await provider.resolveDebugConfiguration(undefined, csharp)).type, 'isoplane', 'a project that gained the SDK between launches');
    assert.deepEqual(asked, [file, file]);
});

test('a project path the editor has yet to substitute is classified by the later hook', async () => {
    const file = path.resolve('app', 'App.csproj');
    const { provider, asked } = providerOf(file);
    const csharp = { type: 'dotnet', request: 'launch', name: 'Debug', projectPath: '${input:directory}/App.csproj' };

    assert.equal(await provider.resolveDebugConfiguration(undefined, csharp), csharp);
    assert.deepEqual(asked, []);
    assert.deepEqual(await provider.resolveDebugConfigurationWithSubstitutedVariables(undefined, { ...csharp, projectPath: file }),
        { type: 'isoplane', request: 'launch', name: 'Debug', project: path.dirname(file), configuration: 'Debug' });
    assert.deepEqual(asked, [file]);
});

test('a variable followed by a parent directory still leaves the project to the later hook', async () => {
    const workspace = path.resolve('workspace');
    const ordinary = path.join(workspace, 'services', 'App.csproj');
    const folder = { uri: { fsPath: workspace } };
    const { provider, asked } = providerOf(path.join(workspace, 'App.csproj'));
    const csharp = { type: 'dotnet', request: 'launch', name: 'Debug', projectPath: '${fileDirname}/../App.csproj' };
    const substituted = { ...csharp, projectPath: path.join(workspace, 'services', 'views', '..', 'App.csproj') };

    assert.equal(await provider.resolveDebugConfiguration(folder, csharp), csharp);
    assert.deepEqual(asked, []);
    assert.equal(await provider.resolveDebugConfigurationWithSubstitutedVariables(folder, substituted), substituted);
    assert.deepEqual(asked, [ordinary]);
});
