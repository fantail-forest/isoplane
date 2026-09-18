---
title: Connect to PostgreSQL
---

The release's `samples/aspnet-todo` application uses ASP.NET, Entity Framework
Core, and Npgsql against a real PostgreSQL database. It requires verified TLS and
turns connection pooling off. The same project supports interpreted and NativeAOT
configurations.

## Use an existing development database

Use an empty development database at your PostgreSQL provider. Obtain its TLS
hostname, port, database name, username and password. Keep the hostname supplied
by the provider: it must match the server certificate.

Run the sample's `schema.sql` using your provider's SQL console or an ordinary
PostgreSQL client. This creates the `Todos` table. Schema administration runs
outside the guest; the application does not run migrations on startup.

A public CA certificate uses the platform's supplied trust roots. For a database
using a private CA, see [local private trust](#local-private-trust) below.

## Configure the sample

Copy `samples/aspnet-todo` from the release into a fresh working directory, then
change into that copy. Set `ConnectionStrings:Todo` using [local development settings](local-settings.md).
For example, replace the values in this connection string with your development
database's values:

```text
Host=YOUR_TLS_HOSTNAME;Port=5432;Database=todo;Username=YOUR_USER;Password=YOUR_PASSWORD
```

The sample enforces `SslMode=VerifyFull` and `Pooling=false` when constructing
its connection string. It reads configuration when creating each request's
DbContext, without connecting to the database during application preparation.

For a normal public database endpoint, remove the sample's
`allow_endpoints = ["127.0.0.1:5432"]` line from `[dev]` in `wisp.toml`.
For a database on your local machine, change that line to its exact local IP
address and port. This explicitly permits that local development destination;
it does not open unrestricted access to private networks.

## Optional: start a disposable local database

If you do not have a development database, the copied sample includes a local
setup helper. It requires Python 3, OpenSSL, Docker with a running daemon you can
access, and the installed .NET tooling from the [installation guide](install.md).
Docker downloads the `postgres:16` image if needed. Add the sample's package feed
as shown above before running it.

From the copied sample directory:

```bash
python3 local-postgres.py start
```

This starts PostgreSQL bound only to a randomly allocated loopback port, creates
the schema, and sets `ConnectionStrings:Todo` in the project's User Secrets. It
uses a random password and a private development CA; the password is not printed.
Plaintext database connections are refused. The certificates expire after seven
days; stop and recreate the database to renew them.

Apply the two configuration snippets printed by the helper to `wisp.toml`:
replace `allow_endpoints` inside `[dev]`, then add the separate `[dev.trust]`
table. These permit precisely the local endpoint and trust its generated CA.
Do not also follow the external-provider connection-string instructions for
this option. You can now run the app as described below.

The database is disposable: stopping it deletes all its rows. After stopping
the application, clean up from the same sample directory:

```bash
python3 local-postgres.py stop
dotnet user-secrets remove ConnectionStrings:Todo
```

Remove the local endpoint and `[dev.trust]` from `wisp.toml` before switching to
another database. The helper removes only its recorded container and temporary
certificate directory. Its ignored `.local-postgres.json` file records that
ownership; keep it until cleanup. Other User Secrets are left untouched.

## Run and exercise the API

```bash
dotnet run -c WispInterpretedDebug
```

Use the URL printed by the command. The following examples use `curl`; another
HTTP client works too. Set `app_url` to the printed address:

```bash
app_url="http://127.0.0.1:PORT_FROM_TERMINAL"
curl "$app_url/todos"
curl --json '{"title":"My first task","done":false}' "$app_url/todos"
```

The first response should be `[]` for an empty database. The second returns the
created TODO with its `id`. Use that ID to update and delete it:

```bash
todo_id=ID_FROM_RESPONSE
curl --request PUT --json '{"title":"Completed task","done":true}' \
  "$app_url/todos/$todo_id"
curl --request DELETE "$app_url/todos/$todo_id"
curl "$app_url/todos"
```

Check the table independently in your database's SQL console. Restart the app
after creating a row to confirm persistence comes from the database.

Stop the application, run `dotnet run -c WispNativeAotRelease`, and repeat the
same operations. The sample generates its EF model and queries at build time
for NativeAOT.

## Local private trust

For a local database with a private CA, obtain the CA certificate from whoever
created that database. Put its PEM certificate in your application's directory,
for example `local-ca.pem`, and calculate its SHA-256:

```bash
sha256sum local-ca.pem
```

Add the result to `wisp.toml`:

```toml
[dev.trust]
bundle = "local-ca.pem"
sha256 = "SHA256_FROM_COMMAND"
```

This selects the local development trust bundle. A CA-only bundle replaces the
normal public roots for that session; other HTTPS destinations may then fail
validation. Use a development bundle containing the roots your application
needs. The hash pins the exact file; update it intentionally when changing the
bundle.

The database certificate still needs a matching hostname or IP address and a
valid chain. Trusting its CA does not disable those checks. Keep its private key
on the database server, not in the application. Restart the development session
after changing local trust settings.

See [connection-pool and eviction behavior](application-lifecycle.md) before
enabling pooling in your own application.
