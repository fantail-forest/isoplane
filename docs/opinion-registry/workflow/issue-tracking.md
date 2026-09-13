# Issue Tracking

## Track an outcome once

Search for an existing issue before creating one. Give each implementation issue one coherent outcome and observable acceptance; a tracker groups those issues rather than restating their acceptance criteria. An issue can support several parent outcomes, but appears only once within each tracker.

**Not this:** Separate tickets for the same socket fix under the runtime, SDK and MVP headings, or a checkbox for each layer with no usable outcome.

**Exception:** A distinct regression or independently deliverable follow-up earns a linked issue; explain what remains rather than duplicating the original scope.

## Make tracking checkboxes point to issues

Use exactly one checkbox per linked issue, grouped under functional subheadings. Put acceptance steps in the child issue; create a child for otherwise untracked work. Use the issue's state for completion, with its closure reason visible so deferred, duplicate or rejected work is not mistaken for delivered functionality.

**Not this:** One checkbox for setup, runtime, docs and testing; repeated links to the same issue; or an unlinked checkbox that nobody can own or discuss independently.

**Exception:** Ordinary implementation issues may have acceptance checklists. They are not tracking issues merely because they contain several criteria.

## Keep milestone scope explicit

A milestone tracker contains required work, completed or pending, and clearly labeled optional work under consideration for that milestone. Keep deferred work and unrelated issue references outside its body; preserve them in the broader backlog or comments. Split mixed-scope issues before starting milestone work. Give the milestone independently closable implementation issues; retain the remaining scope in separate issues or an explicit tracker. Do not schedule or report completion of a portion of an implementation issue.

**Not this:** Listing post-MVP migration as an unchecked MVP requirement, or deleting optional experiments merely because they are not launch gates.

**Exception:** A short exclusion can explain a boundary. It is not another deferred-work checklist.

## Keep bodies useful for the next action

Bodies describe current scope, decisions, dependencies and work still to do. Checkboxes and linked issues provide status; lengthy current-state summaries, test reports and chronology belong in comments. Keep prior context in the body only when it explains remaining work, and preserve evidence in comments when reorganizing a tracker.

**Not this:** Sections organized by the date an agent worked, several generations of contradictory status, or a wall of historical test counts before the checklist.

**Exception:** A concise reproducer or partial implementation fact belongs beside the remaining task when it changes what the next contributor should do.

## Choose the issue shape for the question

Use a concrete title and only the sections needed to make the work actionable. Keep the problem separate from a proposed solution so a narrowly successful workaround does not silently define the product contract.

| Kind | Information needed |
| --- | --- |
| Bug or regression | Expected and actual behavior, reproducible steps or fixture, affected runtime/artifact/environment, and relevant error output. Distinguish a reproduced failure from a source-review suspicion. |
| Feature or integration | Customer outcome, scope and exclusions, current relevant foundation, and observable acceptance through the real interface. |
| API or contract proposal | The consumer's scenario, proposed surface/example, alternatives and unresolved compatibility or lifecycle decisions. No implementation is implied by accepting an investigation. |
| Investigation or experiment | Question to answer, constraints, evidence needed and decision to produce. A justified rejection can complete the investigation without shipping a feature. |
| Tracker | Goal, scope boundaries and functional groups of linked issues. Detailed design and acceptance live in children. |

**Not this:** A universal issue template demanding an estimate, architecture essay and rollout plan for a small defect, or a feature ticket that treats an untested design as settled.

**Exception:** Security-sensitive evidence stays in an appropriately restricted channel rather than being copied into an unsuitable issue.

## Triage with a small, meaningful vocabulary

Classify issues by kind and functional area using existing labels; use milestone membership for release intent and an assignee when someone actually owns the work. Record a blocker as a specific dependency or missing decision. Confirm owner-defined scope before turning a suggestion, probe success or label into a release commitment.

**Not this:** Copying another project's entire label catalog or maintaining several incompatible status systems in labels, prose and checkboxes.

**Exception:** Add a label or workflow state when it answers a recurring coordination question the existing metadata cannot express.

## Close with the outcome and its evidence

Use comments to link the implementation and relevant validation, including exact artifacts and material limits when they affect the conclusion. Close implementation work when its acceptance is met; move remaining independent work to linked issues without implying it passed. Distinguish completed, duplicate, superseded, deferred and rejected dispositions, and update parent checkboxes accordingly. Founder manual acceptance remains separate from automated evidence.

**Not this:** Closing a customer workflow because its runtime fixture passes, or counting a rejected experiment as delivered support.

**Exception:** A duplicate or intentionally abandoned issue may close without implementation, with the reason and replacement recorded explicitly; it does not satisfy an unfulfilled parent requirement.

## Basis in .NET practice

The [.NET runtime issue guide](https://github.com/dotnet/runtime/blob/main/docs/project/issue-guide.md) informs the actionable-work, functional ownership, active assignment and release-intent conventions. Its [contributing guide](https://github.com/dotnet/runtime/blob/main/CONTRIBUTING.md) encourages reusing existing issues and focused discussion. The [bug form](https://github.com/dotnet/runtime/blob/main/.github/ISSUE_TEMPLATE/01_bug_report.yml) asks for reproduction and expected/actual behavior; the [API proposal form](https://github.com/dotnet/runtime/blob/main/.github/ISSUE_TEMPLATE/02_api_proposal.yml) asks for motivation, signatures and usage examples.

Actual .NET trackers, such as [threading](https://github.com/dotnet/runtime/issues/52701) and [ReadyToRun](https://github.com/dotnet/runtime/issues/132805), illustrate outcome-focused grouping but vary in checkbox structure. WISP's one-issue-per-checkbox rule, functional-only grouping and preference for status in comments are explicit owner decisions, not claimed .NET-wide requirements. Keep useful ownership and release distinctions without importing the full large-team label taxonomy or dated process examples.
