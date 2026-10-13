# Writing

Information in this project should be as easy and as pleasant to take in as we
can make it, for the people inside it and the people outside it alike. These
opinions apply to everything written for a reader: customer and internal
documentation, code comments, reports, issues and commit messages.

## Never make the reader decode

Write each sentence, table cell and label so that a reader new to the subject
understands it where it stands, without recalling a convention the document set
up somewhere else. A legend of invented terms, a rating scale or a set of
abbreviations turns reading into two steps, look up then understand, and makes
the reader carry a table of unfamiliar meanings in their head for the length of
the document. That is slow and tedious for a person even when every definition
is clear. Say the thing itself, in words that already mean it.

Language models decode such text without effort, and they write most of the
text here, so the author will not feel this cost. Check for it deliberately:
open the document at any sentence and ask whether it can be understood from
there.

A glossary or legend can still help a reader who wants precision. Keep it as an
aid: beside what it explains or after it, never something to learn first.

**Not this:** A compatibility page that opens with two rating scales, four
grades of "Your code" and seven kinds of "Outlook", and then gives every one of
its 27 rows as a pair of codes: "Session: Small, Isoplane fix". To read one row
the reader has to remember that "Small" means a one-for-one replacement and
that "Isoplane fix" means the SDK can change so that their code need not.
"Small: add one middleware" and "Yes, in Isoplane" say the same and need no
key. Likewise a report whose conclusion is "B beats A under C2 unless R3
holds", which sends the reader to four definitions to read one sentence.

**Exception:** Vocabulary the reader already has, and names the product itself
shows them, such as an error code or a setting. Link those to the one page that
defines them.
