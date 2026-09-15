---
title: Supply local development settings
---

Use .NET User Secrets for settings such as a development database connection
string. Run these commands in the application directory:

```bash
dotnet user-secrets init
dotnet user-secrets set "ConnectionStrings:Todo" "YOUR_DEVELOPMENT_CONNECTION_STRING"
```

Replace the placeholder with your own development connection string. User Secrets
keeps the value outside the project; it is a local development store, not an
encrypted production secret manager. Avoid putting credentials in source files,
`wisp.toml`, or committed configuration. Be mindful that a value typed as a shell
argument can be retained in shell history; `dotnet user-secrets set` also accepts
JSON through standard input.

Start the application with `dotnet run`. The development tools read the project's
User Secrets and make the settings available through ASP.NET's ordinary
`IConfiguration` during preparation and activation. Restart the development session
after changing a setting; the tools regenerate the template for the changed settings.
Secret changes are not live-reloaded.

## Read settings when they are needed

Read settings during `Application.Create` or the initialization hook when they are
needed to prepare reusable state. Values retained there become part of the template.
For request-scoped services, register a factory that resolves configuration when the
service is created:

```csharp
builder.Services.AddDbContext<TodoDb>((services, options) =>
{
    var configuration = services.GetRequiredService<IConfiguration>();
    var connection = configuration.GetConnectionString("Todo")
        ?? throw new InvalidOperationException("Set ConnectionStrings:Todo with dotnet user-secrets.");
    options.UseNpgsql(connection);
});
```

This example uses the EF Core PostgreSQL provider and its `TodoDb` application
context. The [PostgreSQL guide](connect-postgresql.md) shows the complete sample,
including verified TLS and its compiled EF model.

Remove a local value when you no longer need it:

```bash
dotnet user-secrets remove "ConnectionStrings:Todo"
```
