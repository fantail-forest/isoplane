using Isoplane;
using Isoplane.AspNetCore;

var builder = IsoplaneHosting.CreateBuilder(args);
var app = builder.Build();

app.MapGet("/", () => "Hello from Isoplane!");

// Runs during preparation, before Isoplane captures the template that every
// instance starts from. Serving a request here means no instance pays for this route's first use.
Initialization.Initializing += () => app.WarmupAsync("/");

await app.RunAsync();
