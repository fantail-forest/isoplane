using Isoplane;
using Isoplane.AspNetCore;

var builder = IsoplaneHosting.CreateBuilder(args);
var app = builder.Build();
app.MapGet("/", () => "Hello from Isoplane!");
Initialization.Initializing += () => app.WarmupAsync("/");
await app.RunAsync();
