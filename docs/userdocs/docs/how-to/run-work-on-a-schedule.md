---
title: Run work on a schedule
---

import {OnlyOn, PlatformToggle} from '@site/src/components/Platform';

<PlatformToggle />

An instance exists to handle requests, so a `BackgroundService` loop or a job
scheduler inside the application runs only while its instance happens to
exist, and stops silently when the instance is evicted.
[Background work](../concepts/dotnet-runtime-differences.md#background-work)
explains why. The reliable way to run work on a schedule is to put it behind
an endpoint and have something outside the application call that endpoint.
This guide adds such an endpoint, protects it with a secret, and calls it.

## Add the endpoint

The endpoint does one bounded piece of the work and reports what it did. A
`POST` keeps crawlers and prefetchers away from it, and a secret in a header
keeps everyone else away:

```csharp
using System.Security.Cryptography;
using System.Text;

var jobSecret = app.Configuration["Jobs:Secret"]
    ?? throw new InvalidOperationException("Jobs:Secret is not configured.");

app.MapPost("/jobs/cleanup", async (HttpRequest request, CancellationToken cancellation) =>
{
    var given = request.Headers["X-Job-Secret"].ToString();
    if (!CryptographicOperations.FixedTimeEquals(Encoding.UTF8.GetBytes(given), Encoding.UTF8.GetBytes(jobSecret)))
    {
        return Results.Unauthorized();
    }

    // Whatever service does the work, in a batch small enough for one request.
    var deleted = await cleanup.DeleteExpiredAsync(batchSize: 100, cancellation);
    return Results.Ok(new { deleted });
});
```

`FixedTimeEquals` compares the secret without leaking how much of it matched.
An application without ASP.NET does the same in its handler, reading the
header from the request it receives.

## Keep the secret in settings

Generate a secret and keep it in User Secrets for local development, as
[Local development settings](local-settings.md) describes:

<OnlyOn platforms="mac linux">

```bash
dotnet user-secrets set "Jobs:Secret" "$(openssl rand -base64 32)"
```

</OnlyOn>

<OnlyOn platforms="win">

```powershell
$secret = New-Object byte[] 32
$random = [System.Security.Cryptography.RandomNumberGenerator]::Create()
try { $random.GetBytes($secret) }
finally { $random.Dispose() }
dotnet user-secrets set "Jobs:Secret" ([Convert]::ToBase64String($secret))
```

</OnlyOn>

## Keep each call inside the request's allowance

Each request has a
[running-time allowance](../concepts/resource-limits.md#running-time), and
the endpoint is an ordinary request. Rather than doing all the work in one
call, do a bounded batch per call and return how much is left, so the
scheduler can call again until nothing remains. Pass the cancellation token
to the work, and make each batch safe to repeat, since a scheduler that saw
no response will retry.

## Call it on a schedule

Any scheduler that can send an HTTP request with a header will do, from
`cron` running `curl` to a hosted scheduler. Locally, call the endpoint by
hand, with the secret you stored and the port from your launch profile:

```bash
curl -X POST -H "X-Job-Secret: <secret>" http://localhost:5080/jobs/cleanup
```

Without the header, the endpoint answers `401 Unauthorized`.
