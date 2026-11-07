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

# Nomi dei mesi, per le date e la linea del tempo. In minuscolo, come si scrivono
# in italiano.
month-1 = gennaio
month-2 = febbraio
month-3 = marzo
month-4 = aprile
month-5 = maggio
month-6 = giugno
month-7 = luglio
month-8 = agosto
month-9 = settembre
month-10 = ottobre
month-11 = novembre
month-12 = dicembre

# Una data per esteso, "7 gennaio 2025".
date-long = { $day } { $month } { $year }
# Un mese sulla linea del tempo, "maggio 2021".
month-year = { $month } { $year }
# Un intervallo tra due date, "dal 7 gennaio 2025 al 3 agosto 2026".
date-span = dal { $start } al { $end }

# Separatori decimale e delle migliaia per numeri e dimensioni. Scritti come
# stringhe letterali perché Fluent elimina gli spazi attorno a un valore semplice.
number-decimal = { "," }
number-group = { "." }

# Unità per le dimensioni dei file, a passi di 1024.
size-b = B
size-kb = KB
size-mb = MB
size-gb = GB
size-tb = TB
