'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

async function editorExit(completed) {
    const errors = [];
    const processState = {
        argv: ['node', 'run.js', 'suite'],
        env: { ISOPLANE_TEST_CLI: 'prepared-cli' },
        platform: process.platform,
        exitCode: 0
    };
    let options;
    await vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'run.js'), 'utf8'), {
        __dirname,
        process: processState,
        console: { error: error => errors.push(error.message) },
        require(name) {
            if (name !== '@vscode/test-electron') return require(name);
            return {
                async runTests(value) {
                    options = value;
                    if (completed) fs.writeFileSync(value.extensionTestsEnv.ISOPLANE_TEST_RESULT, value.extensionTestsEnv.ISOPLANE_TEST_SUITE);
                }
            };
        }
    });
    return { processState, errors, options };
}

test('an editor exit without completing the tests fails even when the editor reports success', async () => {
    const { processState, errors } = await editorExit(false);
    assert.equal(processState.exitCode, 1);
    assert.deepEqual(errors, ['VS Code exited without completing the suite tests.']);
});

test('completed tests pass and their temporary editor profile is removed', async () => {
    const { processState, errors, options } = await editorExit(true);
    assert.equal(processState.exitCode, 0);
    assert.deepEqual(errors, []);
    assert.equal(fs.existsSync(path.dirname(options.extensionTestsEnv.ISOPLANE_TEST_RESULT)), false);
});
