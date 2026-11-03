---
title: Deploy to your own host
---

Build a portable application bundle in CI or on your development machine, copy
it to your host, and prepare it there. One daemon serves multiple applications on
a shared HTTP port. Each application has its own configured Host names.

## Build the bundle

```bash
isoplane bundle --project /path/to/application --configuration Release --output application.tar
```

The archive contains application assemblies, managed dependencies and content
assets, including `appsettings` files. User Secrets and launch-profile settings
stay on the development machine. Keep secrets out of files included in the bundle.

Copy the archive to the host using your own deployment pipeline. Preparation on
the host does not invoke a project build or require the source files.

## Configure the host

On Linux, install the `isoplaned` binary and the matching SDK runtime files. The node archive
contains the binary, a `runtimes` directory, license notices, checksums, and an optional systemd
service installer. The CLI commands below require `isoplaned` on PATH and must run
as the same operating-system user that owns the host's state.

Create `host.toml`:

```toml
# One explicit local namespace owns all applications on this host.
owner = "local"

# Relative paths resolve beside this configuration file.
# The host creates this directory with owner-only access.
data_dir = "state"

[proxy]
# Put a TLS reverse proxy in front of this listener for public HTTPS.
listen = "127.0.0.1:8080"

[preparation]
# The directory containing inventory.json and the installed SDK runtimes.
runtimes = "/opt/isoplane/current/runtimes"
# These bounds apply while preparing a new application, including replay.
initialization_occupancy_ms = 30000
initialization_wall_ms = 120000
initialization_passes = 8
initialization_log_bytes = 1048576

[[apps]]
# Exact host names, without a scheme, path or port. Requests may include a port.
hosts = ["api.example.com"]
[apps.manifest]
# The name selected by deploy --app.
name = "my-api"
# Optional soft memory allowance in bytes (default: 512 MiB).
# guest_memory_bytes = 536870912

# Optional per-application overrides; omitted fields use the defaults.
# [apps.manifest.resource_limits]
# instances_per_application = 16
# occupancy_ms_per_dispatch = 300000
# instance_open_connections = 32
# pending_futures_per_dispatch = 4096
# package_bytes = 52428800
```

Use the runtime directory from your installation if it differs from the example.
Add another `[[apps]]` block and manifest for each additional application. Host
names must not overlap between applications. All configured applications need a
successful deployment before the daemon starts.

Application limits use the [numeric resource defaults](../concepts/resource-limits.md).
The daemon validates application memory limits against its engine capacity.

## Prepare and start

```bash
isoplane deploy --local --config host.toml --app my-api --bundle application.tar
isoplane daemon --config host.toml
```

A successful deployment selects the prepared application for the next daemon
start. It does not change a daemon already serving traffic. Stop and restart that
daemon to activate a new selection. Redeploy an older bundle to return to its code;
the host does not keep an application version history.

For an installation without the managed CLI, the equivalent host commands are:

```bash
isoplaned deploy --config host.toml --app my-api --bundle application.tar
isoplaned serve --config host.toml
```

Check routing directly:

```bash
curl -H 'Host: api.example.com' http://127.0.0.1:8080/
```

Configure your reverse proxy to preserve the application's Host name. Unknown or
malformed Host values do not select an application. Without a Host header, an
absolute request URL can select its configured hostname; other requests without
a Host do not select an application. TLS termination, public DNS and any routing
between machines belong to your deployment setup.

## Supply application settings

Store application configuration in a flat JSON object whose keys use .NET's
colon-delimited names, for example `{"ConnectionStrings:Main":"..."}`. Keep the file
owned by the daemon user with mode `0600`, outside the application bundle.

Add a reference to `host.toml`:

```toml
[[application_settings]]
application = "my-api"
configuration_file = "my-api.settings.json"
# Optional: permit this app to connect to a specific private IP and port.
# allowed_destinations = ["127.0.0.1:5432"]
```

Private and internal network destinations are blocked by default. Use
`allowed_destinations` for exact IP-and-port exceptions, such as an operator-run
database. These exceptions apply during preparation and execution, and changes
require redeployment along with configuration changes.

Preparation captures these settings in an owner-only file bound to the exact
application, version and prepared artifact. Settings encryption belongs to the
operator's settings store; the host does not provide it. Editing the source file does not change a running application or a
previously prepared selection. Redeploy the bundle to capture new values, then
restart the daemon. Keep the host's state directory private: it contains settings snapshots and
initialized application memory, which can include secrets.

## Replace an application

Build and copy the new bundle, then repeat the deploy command. A failed
preparation leaves the selected application and running traffic unchanged. After
a successful preparation, restart the daemon. Restart ends its active requests
and sessions; it does not migrate them to the replacement.

Each preparation captures the application's initialization state. Prepared
artifacts are specific to the engine, runtime and machine capabilities used to
create them. After a host upgrade, redeploy bundles if startup reports an
incompatible artifact. The daemon refuses incompatible state before serving.

Application logs appear on the daemon's standard output and error streams. Use
your service manager or log collector to retain them. With the supplied systemd
service, use `journalctl -u isoplaned`.
