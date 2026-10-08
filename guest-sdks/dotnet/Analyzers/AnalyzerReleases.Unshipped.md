; Unshipped analyzer release
; https://github.com/dotnet/roslyn-analyzers/blob/main/src/Microsoft.CodeAnalysis.Analyzers/ReleaseTrackingAnalyzers.Help.md

### New Rules

Rule ID | Category | Severity | Notes
--------|----------|----------|-------
ISOPLANE1002 | Isoplane | Warning | A guest application cannot start a thread
ISOPLANE1003 | Isoplane | Warning | A guest application cannot wait synchronously
ISOPLANE1010 | Isoplane | Error | An entry method must be static
ISOPLANE1011 | Isoplane | Error | An [HttpHandler] method must return Task<Response>
ISOPLANE1012 | Isoplane | Error | An [HttpHandler] method takes a request
ISOPLANE1013 | Isoplane | Error | A lifecycle method must return Task
ISOPLANE1014 | Isoplane | Error | A lifecycle method takes no parameters
ISOPLANE1015 | Isoplane | Error | An entry method cannot be generic
ISOPLANE1016 | Isoplane | Error | An entry method cannot be overloaded
ISOPLANE1017 | Isoplane | Error | An application has one method for each entry attribute
ISOPLANE1018 | Isoplane | Error | An entry attribute has no effect in an application with an entry point
ISOPLANE1020 | Isoplane | Error | Run blocks until the application stops
ISOPLANE1021 | Isoplane | Error | RunAsync must be awaited
