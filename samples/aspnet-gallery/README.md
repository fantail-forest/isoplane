# ASP.NET gallery

The ASP.NET implementation of the [shared gallery workload](../minimal-gallery/README.md).
It uses request-scoped EF Core contexts, a compiled model and precompiled queries
against PostgreSQL. Image processing, S3 operations, JSON, and ingest ordering
are compiled from the same source files as the minimal implementation.

The linked guide defines the routes, runtime configuration, object keys, image
behavior, and verification procedure. `../minimal-gallery/Design` owns the sole
EF migration schema. This sample's `Design` project generates its compiled model
and query interceptors; it does not own another schema.

Build with `dotnet build -c WispInterpretedDebug` or
`dotnet build -c WispNativeAotRelease`. Run the shared verifier with
`--name aspnet-gallery --limits-json '{"module_bytes_compressed":52428800,"instance_memory_max":536870912,"cpu_ms_per_http_dispatch":300000,"subrequests_per_dispatch":10000,"pending_futures_per_dispatch":4096}'` and the resulting module path. The
module declares the manifest's 512 MiB memory ceiling; interpreted EF execution
requires more than the SDK's default 128 MiB ceiling for this workload. Each
verification run provisions and removes its own PostgreSQL and S3 containers.
