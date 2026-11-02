'use strict';
const fs = require('node:fs/promises');

async function run() {
    const suite = process.env.ISOPLANE_TEST_SUITE;
    if (!['suite', 'launch'].includes(suite)) throw new Error('The runner must select suite or launch.');
    if (!process.env.ISOPLANE_TEST_RESULT) throw new Error('The runner must name its completion file.');
    await require('./' + suite).run();
    await fs.writeFile(process.env.ISOPLANE_TEST_RESULT, suite);
}

module.exports = { run };
