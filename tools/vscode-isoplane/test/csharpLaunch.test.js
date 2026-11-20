'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const test = require('node:test');
const { isoplaneLaunch } = require('../csharpLaunch.js');
const { projects } = require('./standInSdks.js');

test('a C# launch of an Isoplane project file becomes an Isoplane launch of its directory', async t => {
    const files = projects(t, {
        'App.csproj': '<!-- A leading comment. -->\n<Project Sdk="Isoplane.AspNetCore">\n</Project>\n',
        'Library.csproj': '<Project />'
    });
    const directory = path.dirname(files['App.csproj']);
    const folder = { uri: { fsPath: directory } };
    const named = { type: 'dotnet', request: 'launch', name: 'Debug', projectPath: '${workspaceFolder}/App.csproj' };
    assert.deepEqual(await isoplaneLaunch(folder, named),
        { type: 'isoplane', request: 'launch', name: 'Debug', project: directory, configuration: 'Debug' });
    assert.equal(await isoplaneLaunch(folder, { ...named, projectPath: files['Library.csproj'] }), undefined, 'any other project keeps the C# debugger');
    assert.equal(await isoplaneLaunch(folder, { ...named, noDebug: true }), undefined, 'running without debugging keeps the project run command');
    assert.equal(await isoplaneLaunch(folder, { ...named, request: 'attach' }), undefined);
});

test('a C# launch of the project run command becomes an Isoplane launch of the project it names', async () => {
    const command = { type: 'coreclr', request: 'launch', name: 'Debug', program: 'isoplane' };
    assert.deepEqual(await isoplaneLaunch(undefined, { ...command, args: 'dev --project "/my projects/app" --configuration "Release" --no-watch --no-build' }),
        { type: 'isoplane', request: 'launch', name: 'Debug', project: '/my projects/app', configuration: 'Release' });
    assert.equal((await isoplaneLaunch(undefined, { ...command, program: '/usr/bin/dotnet', args: ['/tools/Isoplane.Cli.dll', 'dev', '--project', '/app'] }))?.project, '/app');
    assert.equal(await isoplaneLaunch(undefined, { ...command, program: '/app/bin/App.dll', args: [] }), undefined, 'any other program keeps the C# debugger');
    assert.equal(await isoplaneLaunch(undefined, { ...command, args: ['deploy', '--project', '/app'] }), undefined);
});
