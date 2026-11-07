# Italian is a best-guess draft in an institutional register, to be reviewed by an
# Italian archivist. Every message and term in en.ftl must exist here too (the
# `catalogs_have_the_same_messages` test enforces it).
#
# Terms name the things indice talks about; `.gender` records the grammatical
# gender so a message that needs an article or agreement can see it.

# One run of a web crawler, a set of captures made together and stored in a WACZ.
-crawl = scansione
    .gender = feminine
# One archived copy of one URL at one moment.
-capture = acquisizione
    .gender = feminine
# A curated, named group of crawls with a finding aid.
-collection = raccolta
    .gender = feminine
# A note a curator or reader attaches to a page or a passage of a page.
-annotation = annotazione
    .gender = feminine
# A person who looks after a collection and writes its description.
-curator = curatore
    .gender = masculine
# A WACZ file indice has indexed and can replay.
-archive = archivio
    .gender = masculine
# An archived web page as indexed for search and replay.
-page = pagina
    .gender = feminine
# Looking for words in the text of archived pages.
-search = ricerca
    .gender = feminine
