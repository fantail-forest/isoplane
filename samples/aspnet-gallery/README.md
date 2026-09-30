# ASP.NET gallery

The ASP.NET implementation of the [shared gallery workload](../minimal-gallery/README.md).
It uses request-scoped EF Core contexts, a compiled model and precompiled queries
against PostgreSQL. Its initialization hook runs `GET /images` through the full
request pipeline before template capture. Image processing, S3 operations, JSON, and ingest ordering
are compiled from the same source files as the minimal implementation.

The linked guide defines the routes, runtime configuration, object keys, image
behavior, and verification procedure. `../minimal-gallery/Design` owns the sole
EF migration schema. This sample's `Design` project generates its compiled model
and query interceptors; it does not own another schema.

Build with `dotnet build -c Debug` or `dotnet build -c Release`. Run `task scenarios:gallery SAMPLE=aspnet-gallery` from the repository root.
Each verification run provisions and removes its own PostgreSQL and S3
containers, checks ingest and retrieval, and verifies that a failing read query
rejects preparation before the application becomes ready.
