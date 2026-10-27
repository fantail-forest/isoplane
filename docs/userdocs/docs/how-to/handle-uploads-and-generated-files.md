---
title: Handle uploads and generated files
---

The file system is read-only, so there is nowhere to put a temporary file,
and an instance's memory is limited and shared by the requests it is handling.
[File system](../concepts/dotnet-runtime-differences.md#file-system) explains
the first and [resource limits](../concepts/resource-limits.md#memory) the
second. This guide handles an upload and a generated file without either:
streaming first, and buffering in memory, with a cap, where streaming won't
do.

## Stream an upload to storage

Pass the request body straight through to wherever the file will live. The
application holds one buffer's worth at a time, however large the file.
Anything that accepts an HTTP `PUT` works as the destination, such as an
object store's pre-signed upload URL:

```csharp
const long MaxUploadBytes = 50 * 1024 * 1024;
var storage = new Uri(app.Configuration["Storage:BaseUrl"]
    ?? throw new InvalidOperationException("Storage:BaseUrl is not configured."));

app.MapPut("/files/{name}", async (string name, HttpRequest request, IHttpClientFactory clients, CancellationToken cancellation) =>
{
    if (request.ContentLength is not { } length)
    {
        return Results.StatusCode(StatusCodes.Status411LengthRequired);
    }
    if (length > MaxUploadBytes)
    {
        return Results.StatusCode(StatusCodes.Status413PayloadTooLarge);
    }

    using var content = new StreamContent(request.Body);
    content.Headers.ContentLength = length;
    content.Headers.ContentType = new(request.ContentType ?? "application/octet-stream");
    using var response = await clients.CreateClient().PutAsync(new Uri(storage, name), content, cancellation);
    return Results.StatusCode((int)response.StatusCode);
});
```

Requiring `Content-Length` lets the endpoint refuse an oversized upload before
reading any of it. For a multipart form, `MultipartReader` reads each section
as a stream in the same way, where `Request.ReadFormAsync()` would buffer the
whole form first.

## Buffer a small upload in memory

When the file has to be inspected or transformed before it goes anywhere,
read it into memory, with a cap you choose. The whole file then sits in the
instance's memory for the length of the request, once for each concurrent
upload, so keep the cap well inside the
[memory limit](../concepts/resource-limits.md#memory):

```csharp
const long MaxBufferedBytes = 10 * 1024 * 1024;

app.MapPut("/images/{name}", async (string name, HttpRequest request, CancellationToken cancellation) =>
{
    if (request.ContentLength is not { } length || length > MaxBufferedBytes)
    {
        return Results.StatusCode(StatusCodes.Status413PayloadTooLarge);
    }

    using var buffer = new MemoryStream((int)length);
    await request.Body.CopyToAsync(buffer, cancellation);
    buffer.Position = 0;
    // Inspect or transform the bytes here, then send them on.
    return Results.Ok();
});
```

## Stream a generated file to the client

Write a generated file to the response as you produce it, rather than
building it first. A CSV export of a large table never holds more than one
row:

```csharp
app.MapGet("/export.csv", async (HttpResponse response, CancellationToken cancellation) =>
{
    response.ContentType = "text/csv";
    await using var writer = new StreamWriter(response.Body, leaveOpen: true);
    await writer.WriteLineAsync("id,name");
    // items is whatever reads the rows, one at a time.
    await foreach (var item in items.ReadAllAsync(cancellation))
    {
        await writer.WriteLineAsync($"{item.Id},{item.Name}".AsMemory(), cancellation);
    }
});
```

A library that wants a file path to write to can usually take a `Stream`
instead. Give it the response body to stream the result, or a `MemoryStream`
with a cap when it has to finish before anything is sent. A generated file
that must outlive the request goes to storage the same way an upload does.
