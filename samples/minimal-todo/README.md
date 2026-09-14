# Minimal PostgreSQL TODO

A TODO application using the Wisp handler API and Npgsql, without ASP.NET or
EF Core in the guest. It uses the same `Todos` table as `../aspnet-todo`;
`../aspnet-todo/schema.sql` is the current shared schema input. There is no
second schema definition in this sample.

| Request | Result |
| --- | --- |
| `GET /todos` | All rows ordered by ID |
| `POST /todos` with `{"title":"example","done":false}` | A new row, initially incomplete |
| `PUT /todos/{id}` with `{"title":"updated","done":true}` | The updated row, or 404 |
| `DELETE /todos/{id}` | 204, or 404 |

Set `ConnectionStrings:Todo` through runtime configuration or .NET User Secrets.
The application requires verified PostgreSQL TLS, disables connection pooling,
and reads credentials only when a request runs. Configure the database endpoint
and trusted CA in `wisp.toml` for local development. Do not put credentials in
that manifest or the source.

Use `dotnet build -c WispInterpretedDebug` or
`dotnet build -c WispNativeAotRelease`, followed by `dotnet run` with the same
configuration. The Wisp SDK publishes `out/Todo.wasm`. The sample's default
local endpoint is `127.0.0.1:5432`; replace it with the actual database endpoint.
Both TODO implementations use the same sample resource limits for comparisons.

The sample declares a 512 MiB maximum guest memory. This ceiling keeps the
workload comparison consistent; it is not a claim about memory actually used.
