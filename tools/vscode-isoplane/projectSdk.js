'use strict';
const { execFile } = require('node:child_process');
const path = require('node:path');

/**
 * Whether MSBuild evaluates the project file, named by absolute path, with an Isoplane SDK, which
 * both Isoplane SDKs mark by setting `IsoplaneSdkVersion`. The evaluation runs no target. A project it cannot evaluate, or one
 * `dotnet` cannot be started for, is not an Isoplane project.
 */
function isIsoplaneProject(file) {
    if (!file.toLowerCase().endsWith('.csproj')) return Promise.resolve(false);
    // By its full path, because MSBuild reads a bare file name that starts with a hyphen as a switch.
    return new Promise(resolve => execFile('dotnet', ['msbuild', file, '-nologo', '-getProperty:IsoplaneSdkVersion'],
        // From the project's directory, so that the .NET SDK its global.json selects evaluates it.
        { windowsHide: true, cwd: path.dirname(file) }, (error, output) => resolve(!error && output.trim().length > 0)));
}

module.exports = { isIsoplaneProject };
