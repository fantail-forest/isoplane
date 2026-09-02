# Guiding Philosophy

Mandatory reading. These principles cut across every concern category and inform every opinion in this registry.

## Pit of Success

The correct path should be the easiest path. Structure code, interfaces, and infrastructure so that developers and AI agents naturally fall into doing the right thing. When a mistake keeps happening, don't fix it by adding documentation or call-site boilerplate; fix the default.

A deep module with a simple interface is a pit of success. A clock that tests must inject is a pit of success. A fixture build that tests refuse to run themselves is a pit of success. A host interface with one definition that every other representation is generated from is a pit of success.

**The test:** if doing the wrong thing requires less effort than doing the right thing, the design is backwards. Fix the design, not the developer.

## Broken Windows

A broken window is any visible signal of neglect: dead code, copy-pasted boilerplate, incomplete abstractions, inconsistent patterns, a config knob nothing reads, a stub that silently succeeds, a comment narrating a bug from months ago. Left unrepaired, broken windows signal "no one cares about this area" and invite further degradation. The damage isn't the defect itself; it's the cascade it triggers as future developers and agents replicate the pattern.

Every change should leave the codebase in a state that signals care. When you encounter a broken window:

1. **Fix it** if the fix is small and safe.
2. **Board it up** if you can't fix it now: raise an issue that says what's needed, when, and why. A boarded-up window signals awareness, not neglect.
3. **Never walk past it silently.** Acknowledge it exists, even if you can't address it in this change.

**The test:** would a new developer reading this code conclude that quality is valued here, or that shortcuts are normal?

## Trendline Reversal

The goal of every change is to make the codebase better, not just consistent. "This is how we already do it" is not a justification for perpetuating a pattern that should be improved. When a better approach exists, new code should follow the better approach even if existing code doesn't.

This means reviews will sometimes flag code that "works" and is "consistent with existing patterns." That's intentional: the reviewer is enforcing the direction, not the status quo.

## Spreading Risk

A finding isn't just about the code in front of you; it's about the pattern that code establishes. Code that works but installs a bad pattern is more damaging over time than a localised bug, because the pattern will be replicated by future developers and agents who treat existing code as a template. This is doubly true when most of the code is written by agents, who read the tree as instruction.

When evaluating a change, ask: "what does this code teach the next reader?" If the answer is "copy this approach," make sure the approach is worth copying.

## Validate Before You Plan

A plan is a hypothesis. It earns the right to be extended only when the previous step has been demonstrated through the interface it changes, on a real machine. Building layer N+1 on an unvalidated layer N produces a tower of code that is internally consistent and externally untested, and the further from the base, the less of it survives contact with reality.

Work is therefore iterative: develop against the current plan, validate by reviewing whether the work achieved the goal and reflecting on the deviations from the plan that were necessary and why, then replan, reworking the path or the goal only when what was learned warrants it. A design document, a specification, or a roadmap is input to the first iteration and nothing more. The plan for any iteration is narrow: what this iteration needs, with longer-range goals present only as the rationale for its decisions. Detailed instructions for iterations that have not been reached yet are speculation, and they age worst of all.

**The test:** for any sentence that says the system does X, can you name the test that fails when it doesn't? If not, the sentence is a plan, and plans don't go in comments, docs, or reference pages.
