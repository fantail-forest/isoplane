---
mdx:
  format: md
---

# Isoplane.Sdk assembly

## Isoplane namespace

| public type | description |
| --- | --- |
| class [ActivatedAttribute](./Isoplane/ActivatedAttribute.md) | Marks the method that runs in each instance before its first request, in an application written without ASP.NET. |
| static class [Activation](./Isoplane/Activation.md) | Lets an application set up state that must differ between instances, as each instance starts. |
| class [HttpHandlerAttribute](./Isoplane/HttpHandlerAttribute.md) | Marks the method that handles every HTTP request in an application written without ASP.NET. |
| static class [Initialization](./Isoplane/Initialization.md) | Lets an application prepare captured state ahead of requests, that every instance then starts with. |
| class [InitializerAttribute](./Isoplane/InitializerAttribute.md) | Marks the method that prepares state every instance shares, in an application written without ASP.NET. |
| class [IsoplaneLoggerProvider](./Isoplane/IsoplaneLoggerProvider.md) | Sends the messages an application writes with .NET's `ILogger` to the application's log. |
| class [Request](./Isoplane/Request.md) | An HTTP request whose whole body is in memory before the handler runs. It is the usual parameter of an [`HttpHandlerAttribute`](./Isoplane/HttpHandlerAttribute.md) method. |
| class [RequestHead](./Isoplane/RequestHead.md) | The parts of an HTTP request other than its body: the method, the URL, the headers, and a token that says when to stop. |
| class [Response](./Isoplane/Response.md) | The HTTP response an [`HttpHandlerAttribute`](./Isoplane/HttpHandlerAttribute.md) method returns. |
| class [ResponseStream](./Isoplane/ResponseStream.md) | Writes a response body in pieces, so the client receives each piece while the handler is still producing the rest. |
| static class [RuntimeConfiguration](./Isoplane/RuntimeConfiguration.md) | The settings Isoplane supplies to the application, such as connection strings and credentials, for an application written without ASP.NET. |
| class [StreamingRequest](./Isoplane/StreamingRequest.md) | An HTTP request whose body is read as a stream while it is still arriving. Declare it as the parameter of an [`HttpHandlerAttribute`](./Isoplane/HttpHandlerAttribute.md) method, in place of [`Request`](./Isoplane/Request.md), to accept uploads too large to hold in memory. |
