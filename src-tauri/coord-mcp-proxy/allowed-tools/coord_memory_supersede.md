`coord_memory_supersede` is IN because withholding it would leave the memory
surface able to INSERT but never CORRECT, and that is worse than withholding
both: the backend dedups on content hash rather than title, so a session
obeying served policy `memory-and-notes` `fix-stale-facts` would write a
SECOND live row and leave the disproven one retrievable beside it. Authority
-wise it grants nothing new — same tenant binding from the same verified JWT,
same private-scope clamp as `coord_memory_record` (already granted), and the
coord tool exposes no delete. Added WITH the tool, for the reason
`../deliberate-exclusions/README.md` gives: the three drift instances in this list were each found
by a human hitting `-32601`, and a correction door that answers `-32601` is
indistinguishable, from inside a session, from one that does not exist.
