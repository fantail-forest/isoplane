# Interfaces

## Make a crate only where there is a real interface

Create a crate when it enforces a useful dependency or visibility boundary, isolates a toolchain or target, or supports independent building and reuse. One consumer can justify that boundary. Judge what the separation prevents callers from depending on, balanced against build and API maintenance costs.

**Not this:** Splitting files into crates without reducing coupling, or making a documentation generator depend on a benchmark crate and compile the engine.

**Exception:** An early extraction can support a known, imminent consumer when the boundary is already clear and cheap to validate. Consumer count alone does not justify it.

## Make a trait a boundary between concerns, not a mirror of one implementation

Isolate infrastructure and external services behind responsibility-focused interfaces. A concrete module can provide that boundary; use a trait when actual substitution, test isolation or ownership needs justify it. Shape the interface around what consumers need, keeping backend machinery private. Do not generalize it for a hypothetical replacement or copy a backend's whole API into a trait.

**Not this:** A trait, fake and conformance suite added solely because a module calls a database, or an unused interface lifted from a specification.

**Exception:** A framework may require a trait even when the application has no substitution need.

## Prefer deep modules over shallow ones

A module, crate, or trait earns its existence by hiding complexity behind a simple interface. If the interface is nearly as complex as the implementation, the boundary is moving code around, not reducing what callers need to know. Depth is what makes a boundary worth crossing.

**Not this:** A storage trait with one method per database operation, so that every caller reassembles the transaction, the fencing check, and the retry it was supposed to be spared.

**Exception:** Boundaries a framework mandates (the bindings implementation the engine requires) exist even when thin. If many such boundaries are thin wrappers over one thing, deepen the thing.

## Minimize call-site boilerplate

If consuming a module requires the same lines at every call site (acquire a lock, unwrap a layered result, check an epoch, open a span), the interface is too shallow and is leaking an implementation concern. Push it behind the boundary so the correct call is the short one.

**Not this:** Every caller of the store fencing its own write; every dispatch path emitting its own completion event by hand.

**Exception:** Boilerplate that is genuinely caller-specific, such as different callers needing different failure handling.

## Prefer to take responsibility over delegating it via a normative statement

When you want to write "callers MUST" or "never call this while", a guarantee the code could hold is being delegated to the reader. Take the responsibility instead: hold the resource in a guard so release is automatic, make the invalid state unrepresentable, or put the sequencing behind the interface. A normative comment survives only where something breaks if the rule is not followed and no restructuring can enforce it, and then it says what breaks.

**Not this:** A doc comment listing the order in which three methods must be called, on a type whose methods could have been one.

**Exception:** A rule that spans a process or file-system boundary the code cannot own, stated with its consequence.

## Apply the deletion test

Imagine deleting the module. If complexity vanishes, it was a pass-through. If complexity reappears across N callers, it was earning its keep. Pass-throughs aren't abstractions; they're a second place to look.

**Not this:** A route table in the control crate that the proxy never reads because the proxy has its own, bridged by an adapter with no non-test callers.

**Exception:** Extraction at a boundary so that the code above it can be tested with a double.

## Attach instrumentation at boundaries and let it nest

Prefer a span guard that records on construction and drop, with context carried on the span rather than in parameters, so that observing a step costs the step nothing in its signature. Boundaries nest, and so do spans: a stage inside a request handler gets its own span under the handler's. When the only way to reach something that must be measured is to pass an observer through, pass it, and treat that as a boundary that wants restructuring when convenient, not a reason to leave the thing unmeasured.

**Not this:** An optional carrier parameter on every function between admission and response, twin instrumented and plain methods, and lock-statistics wrappers on production mutexes, all outliving the benchmark that needed them.

**Exception:** Threading an observer through signatures when no structural means reaches what must be measured.

## Give every implementation a purpose and the shared tests, and remove it when its purpose ends

Several implementations of one interface are normal: the production one, one behind a feature flag, an old and a new one during branch-by-abstraction, a candidate in an experiment. Keep that healthy by giving each a stated purpose, running each through the same conformance tests via the interface it implements, and deleting each at the moment its purpose ends: when the flag is retired, the migration completes, or the experiment picks a winner.

**Not this:** Three queue structures and three placements left selectable by config after the comparison that needed them was over, with production on one and the tests spread across all nine combinations.

**Exception:** None.

## Split files by responsibility

A file that holds the queue, the lifecycle state machine, the socket handling, and its own tests is four files. Size is the symptom; the disease is that a reader cannot find the interface.

**Not this:** A cell module measured in thousands of lines with a table of contents in its header.

**Exception:** None. Consistency with an existing large file is not a reason to grow it.
