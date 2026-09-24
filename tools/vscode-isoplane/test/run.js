'use strict';
const path = require('node:path');
const fs = require('node:fs');
const os = require('node:os');
const { runTests } = require('@vscode/test-electron');
const cachePath = path.resolve(__dirname, '..', '.vscode-test');

if (!process.env.ISOPLANE_TEST_CLI) throw new Error('ISOPLANE_TEST_CLI must name the built Isoplane CLI executable.');
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'isoplane-editor-test-'));
runTests({
    cachePath,
    extensionDevelopmentPath: path.resolve(__dirname, '..'),
    extensionTestsPath: path.resolve(__dirname, 'suite.js'),
    launchArgs: ['--no-sandbox', '--disable-gpu', '--disable-workspace-trust', '--skip-welcome',
        '--user-data-dir=' + path.join(profile, 'user-data'), '--extensions-dir=' + path.join(profile, 'extensions')]
}).catch(error => { console.error(error); process.exitCode = 1; })
    .finally(() => fs.rmSync(profile, { recursive: true, force: true }));
