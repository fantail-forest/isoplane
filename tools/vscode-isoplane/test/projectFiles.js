'use strict';
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/** The marker both Isoplane SDKs set, as a props file a project imports. */
const isoplaneProps = '<Project><PropertyGroup><IsoplaneSdkVersion>1.2.3</IsoplaneSdkVersion></PropertyGroup></Project>';

/** A temporary directory holding `files`, by relative path, removed when the test ends. Returns the directory. */
function directoryOf(t, files) {
    const directory = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'isoplane-projects-')));
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
    for (const [name, content] of Object.entries(files)) {
        fs.mkdirSync(path.dirname(path.join(directory, name)), { recursive: true });
        fs.writeFileSync(path.join(directory, name), content);
    }
    return directory;
}

module.exports = { directoryOf, isoplaneProps };
