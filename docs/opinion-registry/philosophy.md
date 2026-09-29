# Guiding Philosophy

Mandatory reading. These principles cut across every concern category and inform every opinion in this registry.

## Pit of Success

The correct path should be the easiest path. Structure code, interfaces, and infrastructure so that developers and AI agents naturally fall into doing the right thing. When a mistake keeps happening, don't fix it by adding documentation or call-site boilerplate; fix the default.

A deep module with a simple interface is a pit of success. A clock that tests must inject is a pit of success. A shared helper that builds missing or stale fixtures for tests is a pit of success. A host interface with one definition that every other representation is generated from is a pit of success.

**The test:** if doing the wrong thing requires less effort than doing the right thing, the design is backwards. Fix the design, not the developer.

## Broken Windows

Address concrete defects and maintenance friction in the responsibility a change touches. Fix nearby problems when the benefit is clear and the change is small and safe. Report consequential debt outside that scope; create a follow-up when it describes actionable work worth prioritizing. Cosmetic inconsistency alone does not require cleanup or a ticket.

**The test:** does this cleanup reduce a concrete risk or make the current work easier to understand and maintain, enough to justify its review and regression cost?

## Trendline Reversal

Existing patterns are evidence, not automatic justification. Improve a pattern when there is a concrete benefit, while accounting for the cost of maintaining multiple approaches. Follow established project decisions; a personal preference for a different abstraction is not enough to introduce one.

Keep changes within the touched responsibility. When improvement needs a broader migration, describe the tradeoff and proposed scope rather than starting the migration as incidental cleanup.

## Spreading Risk

A finding isn't just about the code in front of you; it's about the pattern that code establishes. Code that works but installs a bad pattern is more damaging over time than a localised bug, because the pattern will be replicated by future developers and agents who treat existing code as a template. This is doubly true when most of the code is written by agents, who read the tree as instruction.

When evaluating a change, ask: "what does this code teach the next reader?" If the answer is "copy this approach," make sure the approach is worth copying.

## Validate Before You Plan

A plan is a hypothesis. It earns the right to be extended only when the previous step has been demonstrated through the interface it changes, on a real machine. Building layer N+1 on an unvalidated layer N produces a tower of code that is internally consistent and externally untested, and the further from the base, the less of it survives contact with reality.

Work is therefore iterative: develop against the current plan, validate by reviewing whether the work achieved the goal and reflecting on the deviations from the plan that were necessary and why, then replan, reworking the path or the goal only when what was learned warrants it. A design document, a specification, or a roadmap is input to the first iteration and nothing more. The plan for any iteration is narrow: what this iteration needs, with longer-range goals present only as the rationale for its decisions. Detailed instructions for iterations that have not been reached yet are speculation, and they age worst of all.

**The test:** for any sentence that says the system does X, can you name the test that fails when it doesn't? If not, the sentence is a plan, and plans don't go in comments, docs, or reference pages.
