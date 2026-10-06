---
title: Connect to PostgreSQL
---

The release's `samples/aspnet-todo` application uses ASP.NET, Entity Framework
Core, and Npgsql against a real PostgreSQL database. It requires verified TLS and
turns connection pooling off. Debug and Release builds use the shared Mono runtime.

To start a new application with a database, `dotnet new isoplane-webapi` creates
one with EF Core migrations, a local database in `compose.yaml`, and tests. This
guide walks through the sample, which adds verified TLS and a compiled EF model.

## Use an existing development database

Use an empty development database at your PostgreSQL provider. Obtain its TLS
hostname, port, database name, username and password. Keep the hostname supplied
by the provider: it must match the server certificate.

Run the sample's `schema.sql` using your provider's SQL console or an ordinary
PostgreSQL client. This creates the `Todos` table. Schema administration runs
outside the guest; the application does not run migrations on startup.

A local session trusts the certificate authorities your machine trusts. For a
database using a private CA, see [local private trust](#local-private-trust) below.

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
DbContext. Its initialization hook warms the read-only `/todos` endpoint, so the
database must be reachable during application preparation. `Program.cs` also sets
EF's `Microsoft.EntityFrameworkCore.Issue31751` switch through `AppContext` so the
compiled model initializes without starting an operating-system thread.

A local session can reach a database on your own machine as well as a public
endpoint; no extra setting permits either.

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

The helper prints the path of its generated CA certificate. Start the application
with that path in `SSL_CERT_FILE`, as the helper shows, so the session trusts the
database's certificate:

```bash
SSL_CERT_FILE=/path/printed/by/the/helper/ca.crt dotnet run -c Debug
```

Do not also follow the external-provider connection-string instructions for
this option.

The database is disposable: stopping it deletes all its rows. After stopping
the application, clean up from the same sample directory:

```bash
python3 local-postgres.py stop
dotnet user-secrets remove ConnectionStrings:Todo
```

The helper removes only its recorded container and temporary
certificate directory. Its ignored `.local-postgres.json` file records that
ownership; keep it until cleanup. Other User Secrets are left untouched.

## Run and exercise the API

```bash
dotnet run -c Debug
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

Stop the application, run `dotnet run -c Release`, and repeat the
same operations. The sample generates its EF model and queries at build time in both configurations.

## Local private trust

A local session trusts the certificate authorities your machine trusts, as any
.NET program does. For a local database with a private CA, obtain the CA
certificate from whoever created that database, then either install it in your
operating system's trust store or name its PEM file for one run:

```bash
SSL_CERT_FILE=/path/to/local-ca.pem dotnet run
```

The database certificate still needs a matching hostname or IP address and a
valid chain. Trusting its CA does not disable those checks. Keep its private key
on the database server, not in the application. Restart the development session
after changing which authorities are trusted.

See [connection-pool and eviction behavior](application-lifecycle.md) before
enabling pooling in your own application.
