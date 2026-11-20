'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const test = require('node:test');
const { isIsoplaneProject } = require('../projectSdk.js');
const { projects } = require('./standInSdks.js');

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
