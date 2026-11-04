# Documentation and Discussions

## Commit information we intend to maintain

Keep architecture explanations, customer documentation, internal checklists and runbooks in `docs/` when we intend to keep them correct as the product changes. Choose the location by the information's useful lifetime and maintenance commitment, not its length or formality.

**Not this:** Committing a current-phase investigation as a permanent reference page merely because it is substantial.

**Exception:** A stable decision and the rationale needed to understand the current design belong in maintained documentation even when an experiment established them.

## Put provisional and expiring material in repository Discussions

Use GitHub Discussions for experiment and research reports, candidate ideas, unresolved questions and conversations whose usefulness is bounded by a current phase or problem. State the question, relevant context, evidence and limits, and what decision or event would make the discussion obsolete. Mark a resolved or superseded discussion accordingly and link its outcome; expiration does not require deleting history.

**Not this:** A growing directory of dated reports that readers must reconcile to discover current behavior, or treating an exploratory Discussion as an approved implementation plan.

**Exception:** Raw logs and large artifacts remain build outputs or attachments. Discussions carry the interpretation and pointers, not an indiscriminate dump of machine output. Do not publish secrets.

## Keep work and durable decisions in their own homes

Issues describe actionable outcomes and acceptance; issue comments hold implementation progress and validation evidence. Link a Discussion when investigation informs an issue. Once a decision is made, update the relevant maintained documentation and link it from the Discussion. Put recurring engineering preferences in the opinion registry; put product requirements in their maintained design or contract. Do not duplicate current-state narratives across all three places.

**Not this:** Using a Discussion as a second task tracker, or copying an entire investigation into an issue body.

**Exception:** A short statement of a settled decision may appear where it explains the remaining work.

## Preserve meaning and links when moving content

Publish and verify the destination before removing a repository report. Preserve evidence, attribution and source revision; update inbound links, navigation and any actionable issue references. Split mixed pages: retain maintained instructions or architecture and move only the temporary report. Avoid leaving a forest of placeholder pages. Check both documentation sites after changes to their content or navigation.

**Not this:** Classifying every page named “design” as temporary, deleting a useful runbook with the experiment it accompanied, or dropping unresolved findings during a move.

**Exception:** A redirect is useful when an externally consumed URL needs to remain valid; it is not a second maintained copy of the report.

## Let documentation improve the product

When writing or testing documentation exposes unnecessary steps, surprising behavior or something difficult to explain, investigate the product and capture a concrete improvement issue when warranted. Do not make workaround prose the default solution. Competitor reviews can identify strengths, weaknesses and questions to investigate, but are inputs to our decisions, not templates or specifications. Evaluate them against Isoplane's actual behavior, audience and other evidence, including reviews of other products.

**Not this:** Copying a competitor's navigation or tutorials and losing the opportunity to notice friction in our own product, or treating one competitor review as the blueprint for our documentation.

**Exception:** Familiar conventions can be adopted deliberately when they serve our users; document and test our workflow rather than inheriting another product's assumptions.
