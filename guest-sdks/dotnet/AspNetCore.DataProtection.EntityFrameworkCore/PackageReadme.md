# Isoplane.AspNetCore.DataProtection.EntityFrameworkCore

Keep the ASP.NET Core Data Protection keys of an Isoplane application in its
Entity Framework Core database. Initialization loads a shared key ring, and
its prepared template retains those keys. Fresh initialization may otherwise
create a different key; reusing a template retains its captured key. Keep old
keys while data protected with them must remain readable.

ASP.NET Core's `PersistKeysToDbContext` does not work in Isoplane's sandbox: it
reads the database synchronously, and a sandboxed application cannot block.
`UseKeysFromDbContext` uses the same `DataProtectionKeys` table and reaches it
asynchronously.

```bash
dotnet add package Isoplane.AspNetCore.DataProtection.EntityFrameworkCore
```

Declare the table in your context, as for `PersistKeysToDbContext`:

```csharp
using Microsoft.AspNetCore.DataProtection.EntityFrameworkCore;
using Microsoft.EntityFrameworkCore;

public sealed class AppDbContext(DbContextOptions<AppDbContext> options)
    : DbContext(options), IDataProtectionKeyContext
{
    public DbSet<DataProtectionKey> DataProtectionKeys => Set<DataProtectionKey>();
}
```

Then, in `Program.cs`:

```csharp
using Isoplane.AspNetCore.DataProtection.EntityFrameworkCore;

builder.Services.AddDataProtection().UseKeysFromDbContext<AppDbContext>();
```

Initialization loads the stored keys and can create and store a key if none is usable. The keys are
read when initialization finishes, so the table must exist by then. An
`Initialization.Initializing` callback that protects data itself, for instance
by warming up a page with a form, calls `await app.LoadDataProtectionKeysAsync()`
first.

It is a package of its own so that `Isoplane.AspNetCore` does not depend on
Entity Framework Core, and an application that does not use it never restores
it. Keep this package at the same version as `Isoplane.AspNetCore`.

Learn more at [isoplane.dev](https://isoplane.dev/docs/concepts/dotnet-runtime-differences#keep-data-protection-keys-across-builds).
