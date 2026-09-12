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

Organize pages around customer tasks. Run tutorial commands against installed
packages from a fresh directory before claiming that the workflow works. Keep
internal paths, repository build steps and unvalidated future commands out of
the guide. Both links and anchors are checked by the static build.
