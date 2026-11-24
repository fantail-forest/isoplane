'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const test = require('node:test');
const { isIsoplaneProject } = require('../projectSdk.js');
const { directoryOf, isoplaneProps } = require('./projectFiles.js');

test('a file that is not a project, or that MSBuild cannot evaluate, is not an Isoplane project', async t => {
    const directory = directoryOf(t, { 'notes.txt': isoplaneProps, 'Broken.csproj': '<Project>' });
    assert.equal(await isIsoplaneProject(path.join(directory, 'notes.txt')), false);
    assert.equal(await isIsoplaneProject(path.join(directory, 'Broken.csproj')), false);
    assert.equal(await isIsoplaneProject(path.join(directory, 'Missing.csproj')), false);
});
