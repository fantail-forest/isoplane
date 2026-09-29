# The Contract Crate

## Keep one definition of the boundary, in the ABI crate

Give every type, constant, and capability that crosses the sandbox boundary one definition, in `isoplane-abi`. How the validator, the linker, and the guest SDKs consume it is their business, and it may change to serve developer experience; what does not change is that anything describing the boundary elsewhere must be generated from that definition or drift-tested against it. Documentation of a contract that is neither is a second contract waiting to drift.

**Not this:** A WIT file whose own header says nothing reads it, alongside the Rust table that is the real list.

**Exception:** None.

## Generate mirrors, or drift-test them

When a language boundary forces a copy of a table (C# import declarations, a documentation page listing capabilities, an MSBuild property holding a limit), produce the copy with a generator from the ABI crate, or keep a test that compares the two and fails on any difference. A hand-maintained copy with a "keep in sync" comment is a bug scheduled for later.

Compare the declarations or values that form the contract. Do not freeze incidental source spelling or treat the presence of an expression in a patch as proof of runtime behavior. Group related drift checks when they share the same inputs; retain real-boundary tests for capability enforcement.

**Not this:** A C# extern list declaring roughly half of the imports by hand, guarded by argument-order comments, with no test that reads both tables.

**Exception:** None.

## One constant, one home

Follow the [configuration guidance](../../../AGENTS.md#environment--configuration): expose a value when there is an operational need to vary it, and keep fixed policy or protocol facts as constants. Give each shared concept one definition in its owning crate. Equal literals do not by themselves establish a shared concept; unrelated policies may need to change independently.

**Not this:** A contract memory ceiling repeated across languages without a drift check, a new configuration knob with no operational use, or coupling unrelated timeouts because their defaults currently match.

**Exception:** A guest-side build property that cannot import Rust may hold the value, provided a test compares it against the ABI crate.

## A variant needs a producer

Add an enum variant, a dispatch kind, or an import only when code constructs it and a test observes it. Adding variants for planned features gives every match arm a branch nobody can test and every reader a feature that does not exist.

**Not this:** A dispatch-kind enum where most variants have no producer anywhere in the tree.

**Exception:** None. Add the variant in the commit that adds the producer.

## Version the wire format

Give records that cross the boundary or land on disk (dispatch records, deltas, manifests) one canonical codec per supported version, a version field, and a round-trip test with a stored golden byte string. Change the version when the format changes and reject unsupported versions explicitly. Keep older decoders only for a concrete supported consumer or explicit data-compatibility requirement, following the [compatibility guidance](../../../AGENTS.md#workflow-develop-validate-replan).

**Not this:** Silently interpreting obsolete bytes as the current format, or maintaining legacy readers for consumers and data the project does not support.

**Exception:** The [pre-release authorization](../../../AGENTS.md#workflow-develop-validate-replan) permits breaking formats without legacy readers or data migration. Update current producers, consumers and internal fixtures together and regenerate affected artifacts; an old fixture does not establish a supported format.
