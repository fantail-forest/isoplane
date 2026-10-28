// Snapshots docs/ as the documentation of a release, under its release line.
// Usage: pnpm run snapshot [version]; the version defaults to the repository's.
import {execFileSync} from 'node:child_process';
import {existsSync, readdirSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import semver from 'semver';

const site = join(dirname(fileURLToPath(import.meta.url)), '..');
const repository = join(site, '..', '..');
const version = process.argv[2] ?? readFileSync(join(repository, 'Directory.Build.props'), 'utf8').match(/<Version>([^<]+)<\/Version>/)?.[1];
if (!semver.valid(version)) throw new Error(`"${version}" is not a release version.`);
// A line is what a breaking release starts: each 0.x minor, then each major.
const line = semver.major(version) < 1 ? `0.${semver.minor(version)}` : `${semver.major(version)}`;

const run = (command, args) => execFileSync(command, args, {cwd: site, stdio: 'inherit'});
run('task', ['--dir', repository, 'docs:limits']);
run('dotnet', ['run', '--project', join(repository, 'tools', 'ApiReference'), '-p:IsoplaneManagedOnly=true', '--',
  'docs/reference', '--since', 'since.json', '--stamp', version, '--line', line]);

// Hand-written markers: `next` becomes this release, and one for a release no
// later than the line's first is dropped, since the line has always had the item.
const {first} = JSON.parse(readFileSync(join(site, 'since.json'), 'utf8'));
for (const entry of readdirSync(join(site, 'docs'), {recursive: true, withFileTypes: true})) {
  if (!entry.isFile() || !/\.mdx?$/.test(entry.name)) continue;
  const file = join(entry.parentPath, entry.name);
  const text = readFileSync(file, 'utf8');
  const resolved = text.replaceAll(':since[next]', `:since[${version}]`).replace(/ ?:since\[([^\]]+)\]/g, (marker, release) => {
    if (!semver.valid(release)) throw new Error(`${file}: "${release}" is not a release version.`);
    return semver.lte(release, first) ? '' : marker;
  });
  if (resolved !== text) writeFileSync(file, resolved);
}

// The line's snapshot is replaced whole: a patch or feature release re-describes its line.
const versions = join(site, 'versions.json');
rmSync(join(site, 'versioned_docs', `version-${line}`), {recursive: true, force: true});
rmSync(join(site, 'versioned_sidebars', `version-${line}-sidebars.json`), {force: true});
const lines = existsSync(versions) ? JSON.parse(readFileSync(versions, 'utf8')) : [];
writeFileSync(versions, JSON.stringify(lines.filter((known) => known !== line)) + '\n');
run(join(site, 'node_modules', '.bin', 'docusaurus'), ['docs:version', line]);
const ordered = JSON.parse(readFileSync(versions, 'utf8')).sort((a, b) => semver.rcompare(semver.coerce(a), semver.coerce(b)));
writeFileSync(versions, JSON.stringify(ordered, null, 2) + '\n');
console.log(`Snapshotted ${version} as line ${line}.`);
