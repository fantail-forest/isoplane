'use strict';
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/** A directory of project files beside stand-ins for the two Isoplane SDKs, which set the marker the real ones set. */
function projects(t, files) {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'isoplane-project-sdk-'));
    const resolved = process.env.MSBuildSDKsPath;
    t.after(() => {
        if (resolved === undefined) delete process.env.MSBuildSDKsPath; else process.env.MSBuildSDKsPath = resolved;
        fs.rmSync(directory, { recursive: true, force: true });
    });
    for (const sdk of ['Isoplane.Sdk', 'Isoplane.AspNetCore']) {
        fs.mkdirSync(path.join(directory, 'sdks', sdk, 'Sdk'), { recursive: true });
        fs.writeFileSync(path.join(directory, 'sdks', sdk, 'Sdk', 'Sdk.props'), '<Project><PropertyGroup><IsoplaneSdkVersion>1.2.3</IsoplaneSdkVersion></PropertyGroup></Project>');
        fs.writeFileSync(path.join(directory, 'sdks', sdk, 'Sdk', 'Sdk.targets'), '<Project />');
    }
    process.env.MSBuildSDKsPath = path.join(directory, 'sdks');
    return Object.fromEntries(Object.entries(files).map(([name, content]) => {
        fs.writeFileSync(path.join(directory, name), content);
        return [name, path.join(directory, name)];
    }));
}

module.exports = { projects };
