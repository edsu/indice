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
# Plural of { -capture }, for counts and lists.
-capture-plural = acquisizioni

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

# Parole comuni dei pulsanti.
action-delete-permanently = Elimina definitivamente

# Etichette delle tabelle di dati nelle pagine delle raccolte e delle scansioni.
meta-curator = Curatore
meta-crawls = Scansioni
meta-size = Dimensione
meta-software = Software
meta-capture-dates = Date di acquisizione
meta-capture-quality = Qualità delle acquisizioni
meta-created = Creata
meta-created-by = Creata da
meta-source = Origine
meta-review = Revisione
meta-browsertrix-item = Elemento Browsertrix
meta-collection = Raccolta
meta-crawl = Scansione
meta-warc-files = File WARC
meta-operator = Operatore
meta-user-agent = User-Agent
meta-robots = Robots
meta-crawl-host = Host della scansione
meta-part-of = Fa parte di
meta-conforms-to = Conforme a
meta-keywords = Parole chiave
meta-license = Licenza
meta-multi-wacz = Multi-WACZ
meta-pages = Pagine
meta-wacz-modified = WACZ modificato
meta-added-by = Aggiunta da
meta-named = Nome originale
meta-sha256 = SHA-256
meta-crawled = Scansionata
meta-indexed = Indicizzata
meta-status = Stato
meta-multi-wacz-value = { $count ->
    [one] { $shown } { -crawl } riunita in un solo file
   *[other] { $shown } { -crawl-plural } riunite in un solo file
}

collection-field-scope = Ambito e contenuto
collection-field-creator = Autore
collection-field-access = Accesso e uso

review-5 = Eccellente
review-4 = Buona
review-3 = Discreta
review-2 = Scarsa
review-1 = Pessima
review-other = Revisionata
review-label = { $word } ({ $rating }/5)

capture-quality = { $count ->
    [one] { $shown } { -capture }, { $pct }% ok
   *[other] { $shown } { -capture-plural }, { $pct }% ok
}
capture-quality-failures = { $summary } — { $codes }
list-and = { $first } e { $last }

crawl-summary-pages = { $count ->
    [one] { $shown } { -page }
   *[other] { $shown } { -page-plural }
}
crawl-summary-by = di { $software }

# Il nome dei campi va in minuscolo in `$fields`; `$Fields` è lo stesso elenco con
# l'iniziale maiuscola, per un messaggio che comincia con l'elenco.
draft-field-description = descrizione
draft-field-narrative = presentazione
draft-field-dates = date
draft-field-subjects = soggetti
draft-fields-unknown = Alcuni campi
drafted-note = Bozza di { $fields } generata con { $model }{ $service }.
drafted-note-reviewed = Bozza di { $fields } generata con { $model }{ $service }, rivista da { $reviewer }.

source-describe-file = Conservato in questo archivio: { $path }
source-describe-url = Letto da { $url }
source-describe-browsertrix = Browsertrix ({ $host }), file { $file }
source-describe-browsertrix-public = Raccolta pubblica Browsertrix ({ $host }), file { $file }
source-badge-remote-title = Ospitata altrove: indice la legge in streaming durante la riproduzione e non ne conserva una copia
source-badge-local-title = Conservata nella cartella dell'archivio

facet-top-sites = Siti principali
facet-by-year = Per anno
facet-types = Tipi
facet-languages = Lingue

collection-replay = Riproduci la { -collection }
collection-edit = Modifica la { -collection }
collection-add-crawls = + Aggiungi { -crawl-plural }
collection-about = Informazioni su questa { -collection }
collection-needed = Ancora da compilare: { $list } (il minimo richiesto per la descrizione archivistica).
collection-edit-link = Modifica questa { -collection }
collection-edit-to-add = per aggiungerli.
collection-edit-to-describe = per descriverla.
collection-empty-nudge = Nessuna descrizione archivistica per ora. Aggiungi gli elementi essenziali per chi legge: chi l'ha raccolta (Autore), perché è stata archiviata (Ambito e contenuto) e chi può usarla (Accesso).
collection-add-with = Aggiungi con
collection-or-edit = oppure modifica
collection-annotations-heading = Annotazioni
collection-annotation-count = { $count ->
    [one] { $shown } nota sulle pagine di questa { -collection }.
   *[other] { $shown } note sulle pagine di questa { -collection }.
}
collection-browse-annotations = Sfoglia le annotazioni
collection-crawls-heading = Scansioni
collection-timeline-label = Pagine acquisite per mese
collection-no-crawls = Nessuna { -crawl } in questa { -collection }.
crawl-file-missing = Il file manca
collection-delete-summary = Elimina questa { -collection }
collection-delete-empty-note = Rimuove la descrizione archivistica di questa { -collection } vuota. L'operazione non può essere annullata.
collection-delete-note = { $count ->
    [one] Questa { -collection } ha { $shown } { -crawl }. Eliminare solo il raggruppamento non è consentito; spunta la casella per eliminare anche la { -crawl } (le sue pagine, il file WACZ e la miniatura). L'operazione non può essere annullata.
   *[other] Questa { -collection } ha { $shown } { -crawl-plural }. Eliminare solo il raggruppamento non è consentito; spunta la casella per eliminare anche le { -crawl-plural } (le loro pagine, i file WACZ e le miniature). L'operazione non può essere annullata.
}
collection-delete-also = { $count ->
    [one] elimina anche la { -crawl } della { -collection }
   *[other] elimina anche tutte le { $shown } { -crawl-plural } della { -collection }
}

crawl-curator-note = Nota del curatore
crawl-provenance = Provenienza
crawl-file = File
crawl-status-present = ✓ presente
crawl-status-missing = ✗ mancante
crawl-pages-heading = Pagine
crawl-no-pages = Nessuna pagina elencata in questa { -crawl }.
crawl-delete-summary = Elimina questa { -crawl }
crawl-delete-note = Rimuove definitivamente questa { -crawl }: le sue pagine dalla ricerca, il file WACZ locale e la miniatura. L'operazione non può essere annullata.

collection-field-dates = Date
collection-field-rights = Diritti
collection-field-subjects = Soggetti
