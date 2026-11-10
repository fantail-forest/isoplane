# Writing

Information here should be easy and pleasant to take in, for internal and
external readers alike: documentation, comments, reports, issues and commit
messages.

## Write text that needs no decoding

Write each sentence, table cell and label so that a reader new to the subject
understands it where it stands. Text that leans on its own legend, rating scale
or abbreviations makes a person look up and then read, holding a table of
unfamiliar meanings in their head: slow and tedious. A model decodes without
effort and will not notice, so check by opening the text at any sentence. A
glossary may sit beside or after the content as an aid.

**Not this:** A page that defines two rating scales and then gives 27 rows as
"Small, Isoplane fix", where "Small: add one middleware" and "Yes, in Isoplane"
need no key. A report concluding "B beats A under C2 unless R3 holds".

**Exception:** Terms the reader already knows, and names the product shows
them, such as an error code or a setting, linked to their definition.

## Give each sentence one job

Decide what a sentence, paragraph or section is there to do for the reader at
that point, and cut anything beyond it. A detail that is true and interesting
but not needed for the job belongs where its benefit is the point, or nowhere.

**Not this:** "Every instance starts as a copy-on-write clone of the
template", in a sentence whose job is to define the template. "A copy" does
the job; copy-on-write belongs where its cost advantage is being explained.

**Exception:** A sentence that exists to sell a benefit may spell it out, with
a concrete comparison and even some redundancy: "a fresh .NET process would
spend its first seconds building its service container; a new instance skips
all of that".

## Define a term where it first appears

Introduce a term in the sentence that first uses it, before using it anywhere
else on the page, and prefer an established term with a reference link over a
paraphrase or a simplified example. Synonyms are welcome once the reader has
been told they name the same thing; two words for one concept, never equated,
read as two concepts.

**Not this:** "preparation" and "initialization" used interchangeably on one
page without saying so; "the thread moves on when a request waits for a
database" where "suspends on an `await`" is the term the rest of the page
uses.

**Exception:** Terms the audience already owns, such as the names of .NET
APIs.

## Name the actor

Say which thing acts, and give it the same name every time. A place, a
concept or a document does not act: the sandbox does not reach the network,
and a page does not say anything. Point to another page with "for X, see
[page]" rather than making the page the subject of a verb, and avoid "says"
in technical prose; "defines" or "describes" is usually the verb meant.

**Not this:** "The application lifecycle says what initialization may do."
"The sandbox cannot reach the file system." Four names for one process on
one page: Isoplane, the host, the runtime, the engine.

**Exception:** None.

## Join clauses with words, not colons

Prefer a connective or a second sentence where a colon would join two
clauses. A colon before an explanation or consequence is a pattern readers now
associate with machine-written text, and it hides the relation between the
clauses that a "because", "so" or "by" would state. This is a preference to
weigh, and an occasional colon that reads well can stay.

**Not this:** "The clock is frozen deliberately: a fine-grained clock is what
side-channel attacks need."

**Exception:** A colon that introduces a list, a code sample or a name, or
that delivers a statement the sentence has just announced ("This goal hinges
on a simple rule: no instance may...").

## Explain concepts in the product's terms

On an explanation page, describe the runtime mechanics and let the reader
infer how their libraries behave. Name a library only where the runtime makes
an exception for it; adapter code written for a framework does not meet that
bar. Otherwise the library's behaviour belongs on the compatibility page, and
the method a hook corresponds to belongs in the how-to or reference for that
framework.

**Not this:** A concept page that defines template initialization as
"`Program.cs` up to `RunAsync`", which is one framework's implementation of
it and wrong for the other supported application style.

**Exception:** A single named example of a concept, where the framework's name
is the clearest way to say which thing is meant.

## Repeat the noun rather than point at it

Where a pronoun could be read against more than one antecedent, or stands in
for a noun from an earlier sentence, repeat the noun. Avoid "one" and "none"
as stand-ins for a noun; they read as numerals, and the reader has to work
out what is being counted.

**Not this:** "The engine refuses to save a template while one is open." "An
instance can be discarded before one happens." Write "while a connection is
open" and "before a collection happens".

**Exception:** A pronoun whose antecedent is the subject of the same
sentence.
