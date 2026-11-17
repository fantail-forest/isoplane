'use strict';
const { execFile } = require('node:child_process');

/** The CLI command that serves as the debug adapter: the program and its arguments. */
function adapterCommand(cliPath) {
    const cli = cliPath || 'isoplane';
    return cli.toLowerCase().endsWith('.dll') ? ['dotnet', [cli, 'debug-adapter']] : [cli, ['debug-adapter']];
}

/**
 * Why the debug adapter cannot serve, in the CLI's words, or undefined when it can.
 * An adapter given no input exits at once, and exits cleanly only when it could have served.
 * The editor hides an adapter's own error output, so a session started without this ends unexplained.
 */
function adapterProblem([program, args]) {
    return new Promise(resolve => {
        const adapter = execFile(program, args, { windowsHide: true },
            (error, _, errors) => resolve(error ? errors.trim().replace(/^isoplane: /, '') || error.message : undefined));
        adapter.stdin.end();
    });
}

module.exports = { adapterCommand, adapterProblem };
