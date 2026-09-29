# The Contract Crate

## Keep one definition of the boundary, in the ABI crate

Give every type, constant, and capability that crosses the sandbox boundary one definition, in `isoplane-abi`. How the validator, the linker, and the guest SDKs consume it is their business, and it may change to serve developer experience; what does not change is that anything describing the boundary elsewhere must be generated from that definition or drift-tested against it. Documentation of a contract that is neither is a second contract waiting to drift.

**Not this:** A WIT file whose own header says nothing reads it, alongside the Rust table that is the real list.

**Exception:** None.

## Generate mirrors, or drift-test them

When a language boundary forces a copy of a table (C# import declarations, a documentation page listing capabilities, an MSBuild property holding a limit), produce the copy with a generator from the ABI crate, or keep a test that compares the two and fails on any difference. A hand-maintained copy with a "keep in sync" comment is a bug scheduled for later.

**Not this:** A C# extern list declaring roughly half of the imports by hand, guarded by argument-order comments, with no test that reads both tables.

**Exception:** None.

## One constant, one home

A limit, a port, or a deadline is configuration with a default; a page size or a header name is a fact. Either way, define the value or its default once, in the crate that owns the concept, and import it everywhere else. Two definitions that agree today are one refactor away from disagreeing silently.

**Not this:** A memory ceiling repeated across a dozen files in four languages; a deploy deadline in three crates; a `now_ms()` helper defined in five.

**Exception:** A guest-side build property that cannot import Rust may hold the value, provided a test compares it against the ABI crate.

## A variant needs a producer

Add an enum variant, a dispatch kind, or an import only when code constructs it and a test observes it. Adding variants for planned features gives every match arm a branch nobody can test and every reader a feature that does not exist.

**Not this:** A dispatch-kind enum where most variants have no producer anywhere in the tree.

**Exception:** None. Add the variant in the commit that adds the producer.

## Version the wire format

Give records that cross the boundary or land on disk (dispatch records, deltas, manifests) one canonical codec per supported version, a version field, and a round-trip test with a stored golden byte string. Change the version when the format changes and reject unsupported versions explicitly. Keep older decoders only while supported consumers or retained data require them.

**Not this:** Silently interpreting obsolete bytes as the current format, or maintaining legacy readers for consumers and data the project does not support.

**Exception:** The [pre-release authorization](../../../AGENTS.md#workflow-develop-validate-replan) permits breaking formats without legacy readers or data migration. Update current producers and consumers together and regenerate affected artifacts.
