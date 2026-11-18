'use strict';
const assert = require('node:assert/strict');
const test = require('node:test');
const { adapterCommand, adapterProblem } = require('../adapterCheck.js');

const node = script => [process.execPath, ['-e', script]];

test('an adapter that refuses to serve is reported in its own words', async () => {
    const refusal = "process.stdin.resume(); process.stdin.on('end', () => { console.error('isoplane: Debugging needs a library. Run isoplane setup.'); process.exit(1); });";
    assert.equal(await adapterProblem(node(refusal)), 'Debugging needs a library. Run isoplane setup.');
});

test('an adapter that serves until its input ends has no problem', async () => {
    assert.equal(await adapterProblem(node("process.stdin.resume(); process.stdin.on('end', () => process.exit(0));")), undefined);
});

test('a CLI that cannot be started is reported', async () => {
    assert.match(await adapterProblem(['isoplane-cli-that-is-not-installed', ['debug-adapter']]), /ENOENT/);
});

test('a CLI assembly runs through dotnet and anything else runs directly', () => {
    assert.deepEqual(adapterCommand('/tools/Isoplane.Cli.dll'), ['dotnet', ['/tools/Isoplane.Cli.dll', 'debug-adapter']]);
    assert.deepEqual(adapterCommand(undefined), ['isoplane', ['debug-adapter']]);
});
