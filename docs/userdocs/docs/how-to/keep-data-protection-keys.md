---
title: Keep ASP.NET Core Data Protection keys across builds
---

import {OnlyOn, PlatformToggle} from '@site/src/components/Platform';

<PlatformToggle />

ASP.NET Core Data Protection encrypts authentication cookies and antiforgery
tokens. A key created during initialization is captured in the template and
shared by its instances. Fresh initialization may create a different key;
reusing a template retains its captured key. This guide keeps keys in the
application's settings or database so that older protected data can still be
read when a new template is prepared.
[Initialization state snapshots](../concepts/dotnet-runtime-differences.md#initialization)
explains why.

## From a secret in the application's settings

```csharp
builder.Services.AddDataProtection().UseConfiguredKeys();
```

This reads `DataProtection:Keys:0`, at least 32 random bytes in Base64, and
refuses to start without it. For local development, use User Secrets:

<OnlyOn platforms="mac linux">

```bash
dotnet user-secrets set "DataProtection:Keys:0" "$(openssl rand -base64 48)"
```

</OnlyOn>

<OnlyOn platforms="win">

```powershell
$key = New-Object byte[] 48
$random = [System.Security.Cryptography.RandomNumberGenerator]::Create()
try { $random.GetBytes($key) }
finally { $random.Dispose() }
dotnet user-secrets set "DataProtection:Keys:0" ([Convert]::ToBase64String($key))
```

</OnlyOn>

To rotate, put the new key at `Keys:0` and the old one at `Keys:1`. The first
protects, all of them unprotect. Remove the old one once nothing it protected
is in use.

## From the application's database

With Entity Framework Core, add
[`Isoplane.AspNetCore.DataProtection.EntityFrameworkCore`](../reference/sdk/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore.md)
at your SDK's version:

```bash
dotnet add package Isoplane.AspNetCore.DataProtection.EntityFrameworkCore --version VERSION
```

Add its `DataProtectionKeys` table to your context, as the underlying
`Microsoft.AspNetCore.DataProtection.EntityFrameworkCore` package describes,
then:

```csharp
using Isoplane.AspNetCore.DataProtection.EntityFrameworkCore;

builder.Services.AddDataProtection().UseKeysFromDbContext<AppDbContext>();
```

Initialization loads the stored keys and can create a key if none is usable. Keys load when
initialization finishes, so the table must exist by then. A migration in an
`Initialization.Initializing` callback is fine. If that callback itself
protects data, for instance by warming up a page with a form, load the keys
first:

```csharp
Initialization.Initializing += async () =>
{
    await database.MigrateAsync();
    await app.LoadDataProtectionKeysAsync();
    await app.WarmupAsync("/");
};
```

The [`isoplane-webapp`](../reference/project-templates.md#isoplane-webapp)
template is set up this way. For another store,
[`UseKeysFrom`](../reference/sdk/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore/DataProtectionDbContextKeys/UseKeysFromDbContext.md)
takes two async functions that read and add keys. A prepared template retains
the key ring loaded during initialization, even after keys expire. When
initialization loads the store, it creates and persists a new key if no
unrevoked active key remains valid beyond the renewal window. The new key uses
the configured key lifetime. Reusing a prepared template neither reloads nor
renews its key ring.

:::note
ASP.NET Core's own stores, such as `PersistKeysToDbContext`, read keys
synchronously and so fail here with `PlatformNotSupportedException`.
:::
