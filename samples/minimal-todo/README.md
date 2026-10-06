# Minimal PostgreSQL TODO

A TODO application using the Isoplane handler API and Npgsql, without ASP.NET or
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
and reads credentials only when a request runs. A local session trusts the
certificate authorities your machine trusts; name a private authority's PEM file
in `SSL_CERT_FILE` for one run. Do not put credentials in the source.

Use `dotnet build -c Debug` or `dotnet build -c Release`, followed by
`dotnet run` with the same configuration.
