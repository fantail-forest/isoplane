# Isoplane customer guides

Customer-facing documentation for the Isoplane SDK and tools.

```bash
cd docs/userdocs
npm ci
npm start        # http://localhost:3123
npm run build
npm run serve
```

The servers bind loopback by default. Pass `-- --host 0.0.0.0` to make a local
preview accessible from another machine. Nothing is published by these commands.

Follow [the Diátaxis opinion](../opinion-registry/frameworks/docusaurus.md):
keep tutorials, how-to guides, generated reference and explanation distinct.
The sidebar groups pages by these needs; each page has one primary purpose. Run
tutorial commands against installed packages from a fresh directory before claiming that the workflow works. Keep
internal paths, repository build steps and unvalidated future commands out of
the guide. Both links and anchors are checked by the static build.
