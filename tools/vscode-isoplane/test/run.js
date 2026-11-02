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
const result = path.join(profile, 'result');
runTests({
    cachePath,
    version: process.env.ISOPLANE_TEST_VSCODE_VERSION || 'stable',
    extensionDevelopmentPath: path.resolve(__dirname, '..'),
    extensionTestsPath: path.resolve(__dirname, 'index.js'),
    extensionTestsEnv: { ISOPLANE_TEST_SUITE: suite, ISOPLANE_TEST_RESULT: result },
    launchArgs: ['--no-sandbox', '--disable-gpu', '--disable-workspace-trust', '--skip-welcome',
        '--user-data-dir=' + path.join(profile, 'user-data'), '--extensions-dir=' + path.join(profile, 'extensions')]
}).then(() => {
    if (!fs.existsSync(result) || fs.readFileSync(result, 'utf8') !== suite) {
        throw new Error(`VS Code exited without completing the ${suite} tests.`);
    }
}).catch(error => { console.error(error); process.exitCode = 1; })
    .finally(() => fs.promises.rm(profile, { recursive: true, force: true, maxRetries: 10 }));
