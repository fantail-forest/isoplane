'use strict';
const path = require('node:path');
const fs = require('node:fs');
const os = require('node:os');
const { runTests } = require('@vscode/test-electron');
const cachePath = path.resolve(__dirname, '..', '.vscode-test');

if (!process.env.ISOPLANE_TEST_CLI) throw new Error('ISOPLANE_TEST_CLI must name the built Isoplane CLI executable or assembly.');
const suite = process.argv[2] || 'suite';
if (!['suite', 'launch'].includes(suite)) throw new Error('Choose suite or launch.');
if (suite === 'launch' && !process.env.ISOPLANE_TEST_PROJECT) throw new Error('ISOPLANE_TEST_PROJECT must name the prepared debugger test application.');
// Unix sockets inside the editor profile need a short pathname.
const temporary = process.platform === 'win32' ? os.tmpdir() : '/tmp';
const profile = fs.mkdtempSync(path.join(temporary, 'isoplane-editor-test-'));
runTests({
    cachePath,
    extensionDevelopmentPath: path.resolve(__dirname, '..'),
    extensionTestsPath: path.resolve(__dirname, suite + '.js'),
    launchArgs: ['--no-sandbox', '--disable-gpu', '--disable-workspace-trust', '--skip-welcome',
        '--user-data-dir=' + path.join(profile, 'user-data'), '--extensions-dir=' + path.join(profile, 'extensions')]
}).catch(error => { console.error(error); process.exitCode = 1; })
    .finally(() => fs.promises.rm(profile, { recursive: true, force: true, maxRetries: 10 }));
