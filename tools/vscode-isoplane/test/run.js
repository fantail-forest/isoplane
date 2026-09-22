'use strict';
const path = require('node:path');
const { runTests } = require('@vscode/test-electron');

if (!process.env.ISOPLANE_TEST_CLI) throw new Error('ISOPLANE_TEST_CLI must name the built Isoplane CLI executable.');
runTests({
    extensionDevelopmentPath: path.resolve(__dirname, '..'),
    extensionTestsPath: path.resolve(__dirname, 'suite.js'),
    launchArgs: ['--no-sandbox', '--disable-gpu', '--disable-workspace-trust', '--skip-welcome']
}).catch(error => { console.error(error); process.exitCode = 1; });
