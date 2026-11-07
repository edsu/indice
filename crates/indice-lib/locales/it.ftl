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

# Plural of { -crawl }, for counts and lists.
-crawl-plural = scansioni
# Plural of { -collection }, for counts and lists.
-collection-plural = raccolte
# Plural of { -page }, for counts and lists.
-page-plural = pagine

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

# La riga sotto il nome dell'archivio nella home, quando chi gestisce l'archivio
# non ha impostato `site.tagline`. Viene interpretata come Markdown: solo testo.
site-tagline-default = Ricerca e consultazione di archivi web

# L'intestazione della pagina, su ogni pagina.
layout-manage = Gestione
layout-search-button = Cerca
layout-signed-in-as = connesso come
layout-logout = Esci
layout-login = Accedi

action-replay = Riproduci
action-edit = Modifica

search-placeholder-all = { search-all-collections }…
search-placeholder-scoped = Cerca in { $scope }…
home-search-placeholder = Cerca nelle pagine archiviate…

# I "Suggerimenti per la ricerca". Gli esempi (`year:2021`) sono sintassi di
# ricerca e restano invariati; si traducono solo le parole intorno.
search-tips-summary = Suggerimenti per la ricerca
search-tips-intro = Scrivi delle parole per cercare nei titoli, nelle intestazioni, nel testo, nelle descrizioni, nelle parole chiave, negli autori e negli URL delle pagine.
search-tips-all-words = Tutte le parole devono corrispondere
search-tips-all-words-after = trova le pagine che le contengono entrambe.
search-tips-phrase = una frase esatta (usa le virgolette)
search-tips-either = l'una o l'altra parola
search-tips-exclude = contiene "climate", esclude "policy"
search-tips-group = raggruppa con le parentesi
search-tips-title = cerca solo nel titolo della pagina
search-tips-author = cerca nell'autore della pagina
search-tips-site = un intero sito, sottodomini compresi
search-tips-domain = solo quel dominio esatto
search-tips-collection = solo le pagine di quella { -collection }
search-tips-or = oppure
search-tips-year = filtra per anno di acquisizione
search-tips-month = filtra per mese di acquisizione
search-tips-modified = filtra per anno di Last-Modified
search-tips-type = solo PDF (oppure
search-tips-lang = solo le pagine in quella lingua
search-tips-status = filtra per stato HTTP (oppure
search-tips-boost = dà più peso alle corrispondenze con "climate"
search-tips-note-intro = Le ricerche non distinguono maiuscole e minuscole. Le corrispondenze nel titolo hanno la precedenza su quelle nel testo.
search-tips-note-needs = richiede il dominio esatto (ad es.
search-tips-note-loose = ); per cercare le parole del dominio in modo più ampio, scrivile semplicemente (ad es.

home-browse-year = Sfoglia per anno
home-top-sites = Siti principali
home-collections = Raccolte
home-new-collection = + Nuova { -collection }
home-empty-cta = Aggiungi una { -crawl } per iniziare la tua prima { -collection }.
home-add-crawl = Aggiungi una { -crawl }
home-no-collections = Non ci sono ancora { -collection-plural }. Aggiungi una { -crawl } con
home-card-crawls = { $count ->
    [one] { $shown } { -crawl }
   *[other] { $shown } { -crawl-plural }
}
source-stored-and-streamed = Conservata qui e in streaming
source-streamed = In streaming
source-stored = Conservata qui

search-nothing-matched = Nessun risultato per
search-results-count = { $count ->
    [one] { $shown } risultato
   *[other] { $shown } risultati
}
search-results-for = per
search-page-of = Pagina { $page } di { $total }
search-filters = Filtri:
search-timeline-label = Risultati per mese di acquisizione
timeline-pages-count = { $count ->
    [one] { $shown } { -page }
   *[other] { $shown } { -page-plural }
}
search-all-collections = Cerca in tutte le { -collection-plural }
search-without = Cerca senza { $label }: { $value }
search-try-fewer = Prova con meno parole o controlla l'ortografia.
search-browse-link = Sfoglia le { -collection-plural } per anno o per sito
search-badge-collection = Raccolta
search-badge-note = Nota
search-note-by = di { $author }
search-captured-times = { $count ->
    [one] (acquisita { $shown } volta)
   *[other] (acquisita { $shown } volte)
}
search-in-collection = in
search-previous = Precedente
search-next = Successiva
search-refine = Affina
search-show-more = Mostra altri { $shown }
search-status-title = Acquisita con HTTP { $code } { $reason }
search-status-title-bare = Acquisita con HTTP { $code }
http-status-301 = Spostata in modo permanente
http-status-302 = Trovata (reindirizzamento)
http-status-307 = Reindirizzamento
http-status-308 = Reindirizzamento
http-status-400 = Richiesta non valida
http-status-401 = Non autorizzato
http-status-403 = Accesso negato
http-status-404 = Non trovata
http-status-410 = Non più disponibile
http-status-429 = Troppe richieste
http-status-500 = Errore interno del server
http-status-502 = Gateway non valido
http-status-503 = Servizio non disponibile
http-status-504 = Timeout del gateway
