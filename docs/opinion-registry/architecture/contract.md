# The Contract Crate

## Keep one definition of the boundary, in the ABI crate

Give every type, constant, and capability that crosses the sandbox boundary one definition, in `isoplane-abi`. How the validator, the linker, and the guest SDKs consume it is their business, and it may change to serve developer experience; what does not change is that anything describing the boundary elsewhere must be generated from that definition. A description that is not generated is a second contract waiting to drift.

**Not this:** A WIT file whose own header says nothing reads it, alongside the Rust table that is the real list.

**Exception:** None.

## Generate mirrors

When a language boundary forces a copy of a table (C# import declarations, a documentation page listing capabilities, an MSBuild property holding a limit), produce the copy with a generator from the ABI crate. The generated copy may be committed. [Regenerating it before each commit](../workflow/tooling.md#regenerate-generated-files-instead-of-testing-them) keeps it current, so no test compares it with its source. Retain real-boundary tests for capability enforcement.

The host interface is always generated. For any other table, a hand-kept copy with a comparing test is the last resort, for a copy that cannot be generated; say beside the copy why. Such a test compares the declarations or values that form the contract, not incidental source spelling.

**Not this:** A C# extern list declaring roughly half of the imports by hand, guarded by argument-order comments, with no generator producing them.

**Exception:** None.

## One constant, one home

Follow the [configuration guidance](../../../AGENTS.md#environment--configuration): expose a value when there is an operational need to vary it, and keep fixed policy or protocol facts as constants. Give each shared concept one definition in its owning crate. Equal literals do not by themselves establish a shared concept; unrelated policies may need to change independently.

**Not this:** A contract memory ceiling repeated by hand across languages, a new configuration knob with no operational use, or coupling unrelated timeouts because their defaults currently match.

**Exception:** A guest-side build property that cannot import Rust takes the value from a file generated from the ABI crate.

## A variant needs a producer

Add an enum variant, a dispatch kind, or an import only when code constructs it and a test observes it. Adding variants for planned features gives every match arm a branch nobody can test and every reader a feature that does not exist.

**Not this:** A dispatch-kind enum where most variants have no producer anywhere in the tree.

**Exception:** None. Add the variant in the commit that adds the producer.

## Version the wire format

For durable or independently exchanged records, define how versions are identified and unsupported versions rejected. Use canonical codecs and golden-byte tests where stable encoding matters; their maintenance cost is less useful for ephemeral records whose producers and consumers change together. Keep older decoders only for a concrete supported consumer or explicit data-compatibility requirement, following the [compatibility guidance](../../../AGENTS.md#workflow-develop-validate-replan).

**Not this:** Silently interpreting obsolete bytes as the current format, or maintaining legacy readers for consumers and data the project does not support.

**Exception:** The [pre-release authorization](../../../AGENTS.md#workflow-develop-validate-replan) permits breaking formats without legacy readers or data migration. Update current producers, consumers and internal fixtures together and regenerate affected artifacts; an old fixture does not establish a supported format.
