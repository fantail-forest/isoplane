---
mdx:
  format: md
---

# Isoplane.Sdk assembly

## Isoplane namespace

| public type | description |
| --- | --- |
| class [ActivatedAttribute](./Isoplane/ActivatedAttribute.md) | Marks the static method that runs once in each instance, before its first request. |
| static class [Activation](./Isoplane/Activation.md) | The moment an instance first runs after activation from its template. Every activation is a copy of the memory pre-initialisation left, so a generator that seeded itself before the snapshot would replay the same output in each copy; subscribe here to reseed or recreate it. The event is raised once per activation, before the first dispatch, and not again after a wake from hibernation, whose state is the instance's own. |
| class [BindingException](./Isoplane/BindingException.md) | A capability call the host refused or could not complete; [`IsoplaneError`](./Isoplane/BindingException/IsoplaneError.md) says which. |
| class [Ctx](./Isoplane/Ctx.md) | The per-dispatch context. |
| static class [DeploymentConfiguration](./Isoplane/DeploymentConfiguration.md) | Non-secret settings fixed for an application version at deployment. |
| class [HttpHandlerAttribute](./Isoplane/HttpHandlerAttribute.md) | Marks the static method that handles every HTTP request to an application that has no entry point. |
| interface [IIsoplaneRouter](./Isoplane/IIsoplaneRouter.md) | What routes a decoded dispatch to guest code. `Exports` is generic over one of these, so the SDK names no tenant type. |
| static class [Initialization](./Isoplane/Initialization.md) | Initializes reusable application state before its deployment template is captured. |
| class [InitializerAttribute](./Isoplane/InitializerAttribute.md) | Marks the static method that prepares reusable state before the application's template is captured. |
| class [IsoplaneLoggerProvider](./Isoplane/IsoplaneLoggerProvider.md) | Writes .NET application logs through the sandbox's bounded host logging capability. |
| interface [IStreamingHttpRouter](./Isoplane/IStreamingHttpRouter.md) | Explicitly selects HTTP routes that consume uploads without SDK buffering. |
| class [Request](./Isoplane/Request.md) | An HTTP request delivered by the host, for any HTTP method. |
| class [RequestHead](./Isoplane/RequestHead.md) | HTTP request metadata, independent of whether the body is buffered or streamed. |
| class [Response](./Isoplane/Response.md) | A buffered or completed streaming response. |
| class [ResponseStream](./Isoplane/ResponseStream.md) | A response whose headers are published immediately and whose writes await host acceptance. |
| static class [RuntimeConfiguration](./Isoplane/RuntimeConfiguration.md) | Deployment-bound application settings supplied during preparation and activation. |
| class [StreamingRequest](./Isoplane/StreamingRequest.md) | A request whose upload can arrive while its handler is running. |
