'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const test = require('node:test');
const { isIsoplaneProject } = require('../projectSdk.js');

/** A directory of project files beside stand-ins for the two Isoplane SDKs, which set the marker the real ones set. */
function projects(t, files) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'isoplane-project-sdk-'));
    const resolved = process.env.MSBuildSDKsPath;
    t.after(() => {
        if (resolved === undefined) delete process.env.MSBuildSDKsPath; else process.env.MSBuildSDKsPath = resolved;
        fs.rmSync(directory, { recursive: true, force: true });
    });
    for (const sdk of ['Isoplane.Sdk', 'Isoplane.AspNetCore']) {
        fs.mkdirSync(path.join(directory, 'sdks', sdk, 'Sdk'), { recursive: true });
        fs.writeFileSync(path.join(directory, 'sdks', sdk, 'Sdk', 'Sdk.props'), '<Project><PropertyGroup><IsoplaneSdkVersion>1.2.3</IsoplaneSdkVersion></PropertyGroup></Project>');
        fs.writeFileSync(path.join(directory, 'sdks', sdk, 'Sdk', 'Sdk.targets'), '<Project />');
    }
    process.env.MSBuildSDKsPath = path.join(directory, 'sdks');
    return Object.fromEntries(Object.entries(files).map(([name, content]) => {
        fs.writeFileSync(path.join(directory, name), content);
        return [name, path.join(directory, name)];
    }));
}

test('a project is an Isoplane project however its file spells the SDK', async t => {
    const files = projects(t, {
        'double.csproj': '<Project Sdk="Isoplane.Sdk" />',
        'single.csproj': "<Project Sdk='Isoplane.AspNetCore' />",
        'element.csproj': '<Project><Sdk Name="Isoplane.Sdk" /></Project>',
        'imports.csproj': '<Project><Import Project="Sdk.props" Sdk="Isoplane.AspNetCore" /><Import Project="Sdk.targets" Sdk="Isoplane.AspNetCore" /></Project>'
    });
    for (const [name, file] of Object.entries(files)) assert.equal(await isIsoplaneProject(file), true, name);
});

test('a project that only mentions an Isoplane SDK is not an Isoplane project', async t => {
    const files = projects(t, {
        'comment.csproj': '<Project>\n  <!-- <Project Sdk="Isoplane.Sdk"> -->\n</Project>',
        'property.csproj': '<Project><PropertyGroup><Description>Sdk="Isoplane.Sdk"</Description></PropertyGroup></Project>',
        'stock.csproj': '<Project />'
    });
    for (const [name, file] of Object.entries(files)) assert.equal(await isIsoplaneProject(file), false, name);
});

test('a file that is not a project, cannot be read or cannot be evaluated is not an Isoplane project', async t => {
    const files = projects(t, {
        'notes.txt': '<Project Sdk="Isoplane.Sdk" />',
        'broken.csproj': '<Project Sdk="Isoplane.Sdk">'
    });
    assert.equal(await isIsoplaneProject(files['notes.txt']), false);
    assert.equal(await isIsoplaneProject(files['broken.csproj']), false);
    assert.equal(await isIsoplaneProject(path.join(path.dirname(files['notes.txt']), 'missing.csproj')), false);
});
