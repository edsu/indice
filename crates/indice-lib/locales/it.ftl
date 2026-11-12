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
# The whole web archive this indice instance serves, as its readers see it.
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
layout-signed-in-as = accesso effettuato come
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
# I nomi dei filtri di ricerca, nella colonna "Affina" e sulle etichette dei filtri
# attivi. La chiave termina con il nome del filtro nella query (`year:` è `filter-year`).
filter-collection = Raccolta
filter-year = Anno
filter-site = Sito
filter-type = Tipo
filter-lang = Lingua
filter-month = Mese
filter-domain = Dominio
filter-status = Stato
filter-modified = Ultima modifica
filter-crawl = Scansione
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
http-status-401 = Non autorizzata
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
meta-crawled = Eseguita
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
review-other = Esaminata
review-label = { $word } ({ $rating }/5)

capture-quality = { $count ->
    [one] { $shown } { -capture }, { $pct }% ok
   *[other] { $shown } { -capture-plural }, { $pct }% ok
}
capture-quality-failures = { $summary } ({ $codes })
list-and = { $first } e { $last }

crawl-summary-pages = { $count ->
    [one] { $shown } { -page }
   *[other] { $shown } { -page-plural }
}
crawl-summary-by = di { $software }

# Il nome dei campi va in minuscolo in `$fields`; `$Fields` è lo stesso elenco con
# l'iniziale maiuscola, per un messaggio che comincia con l'elenco. I nomi sono tra
# virgolette perché l'elenco si unisce con "e" e "ambito e contenuto" ne contiene
# già una.
draft-field-description = “descrizione”
draft-field-narrative = “ambito e contenuto”
draft-field-dates = “date”
draft-field-subjects = “soggetti”
draft-fields-unknown = alcuni campi
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

crawl-name-dated = Scansione del { $date }
crawl-name-dated-time = Scansione del { $date }, { $time }
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

# Parole comuni dell'area di lavoro.
crumb-home = Home
action-cancel = Annulla

# Frasi che il server dice quando una richiesta non riesce. Il testo di un errore
# sottostante (`$detail`) è l'inglese di indice e viene riportato invariato.
error-server = Si è verificato un errore sul server
error-reference = { $message } (riferimento { $id }).
error-no-reference = { $message }.
error-request-failed = Richiesta non riuscita: { $detail }
error-query-too-long = query di ricerca troppo lunga: { $bytes } byte, limite { $limit }
error-index-busy = L'indice di ricerca è occupato ({ $holder }). La modifica non è stata salvata: riprova tra poco.
error-busy = indice è occupato; riprova tra un momento
error-too-many-requests = troppe richieste da questo indirizzo; riprova tra { $secs } s
error-too-many-streams = troppi flussi di archivio aperti da questo indirizzo; riprova tra { $secs } s
error-collection-required = la { -collection } è obbligatoria
error-location-not-url = aggiungi un archivio tramite un URL http(s), oppure carica il file. Un percorso sul disco del server non è accettato qui; per quello usa `indice index` sulla macchina stessa.
error-file-required = il file è obbligatorio
error-org-required = l'organizzazione è obbligatoria
error-select-crawl = seleziona almeno una { -crawl }
error-text-field-long = un campo di testo in questo caricamento supera { $max } byte
error-text-field-utf8 = un campo di testo non è UTF-8 valido
error-upload-refused = caricamento rifiutato: { $detail }
error-job-unknown = operazione sconosciuta o già consumata
error-collection-unknown = { -collection } sconosciuta
error-collection-name-required = il nome della { -collection } è obbligatorio
error-collection-name-too-long = il nome della { -collection } è troppo lungo
error-collection-not-found = { -collection } non trovata
error-crawl-not-found = { -crawl } non trovata
error-annotation-not-found = { -annotation } non trovata
error-replay-link = Questo non è un collegamento a una pagina archiviata
error-note-empty = la nota è vuota
error-note-target-required = l'URL e la data sono obbligatori
error-annotation-none = nessuna { -annotation } corrispondente
error-annotation-not-yours = non è una tua { -annotation }
error-browsertrix-unconfigured = L'importazione da Browsertrix non è configurata: imposta BROWSERTRIX_TOKEN (oppure BROWSERTRIX_USER + BROWSERTRIX_PASSWORD) nell'ambiente del server.
error-archiveit-unconfigured = L'importazione da Archive-It non è configurata: imposta ARCHIVEIT_USER + ARCHIVEIT_PASSWORD nell'ambiente del server.
error-browsertrix = Errore di Browsertrix: { $detail }
error-archiveit = Errore di Archive-It: { $detail }
error-browsertrix-resolver = L'importazione in streaming da Browsertrix richiede il resolver (credenziali).
error-reload-failed = indicizzato, ma il ricaricamento del motore di ricerca non è riuscito: { $detail }
error-lock-failed = impossibile bloccare l'indice di ricerca: { $detail }
error-job-failed = operazione non riuscita: { $detail }
error-collection-not-empty = { $count ->
    [one] Questa { -collection } contiene { $shown } { -crawl }: spunta la casella per eliminare anche questa, oppure eliminala o spostala prima.
   *[other] Questa { -collection } contiene { $shown } { -crawl-plural }: spunta la casella per eliminare anche queste, oppure eliminale o spostale prima.
}

# Perché una persona autenticata non può fare qualcosa. I messaggi `denied-need-`
# completano "il tuo account non è autorizzato a ...", quindi sono all'infinito.
denied-heading = Non puoi modificare questo archivio
denied-title = Operazione non autorizzata
denied-back = Torna all'archivio
denied-page-insufficient = Il tuo account non è autorizzato a { $what }.
denied-insufficient = vietato: il tuo account non è autorizzato a { $what }
denied-unauthenticated = vietato: questa azione richiede l'accesso tramite il proxy frontale
denied-stale = vietato: l'accesso è scaduto; ricarica la pagina per accedere di nuovo
denied-need-curating = curare questo archivio
denied-need-administering = amministrare questo archivio
denied-need-delete-others-crawl = eliminare una { -crawl } aggiunta da un'altra persona

# Perché un versamento è stato rifiutato. `$cap`, `$used` e `$left` sono dimensioni.
refused-too-large = supera il limite di { $cap } per un singolo caricamento (uploads.max_mb in config.yaml)
refused-archive-full = l'archivio è limitato a { $cap } e ne contiene { $used }, quindi restano { $left } (uploads.max_total_gb in config.yaml)
refused-measure = impossibile misurare lo spazio occupato dall'archivio: { $detail }

room-per-upload = Caricamenti fino a { $cap } ciascuno.
room-full = L'archivio è pieno ({ $used } su un limite di { $cap }): caricamenti e importazioni con download sono rifiutati finché non viene rimossa una { -crawl } o aumentato il limite. L'aggiunta tramite URL funziona ancora.
room-left = L'archivio contiene { $used } su un limite di { $cap }; ne restano { $left }.
room-both = { $upload } { $archive }

# Il modulo della descrizione archivistica.
form-new-heading = Nuova { -collection }
form-eyebrow = Descrizione archivistica
form-edit-heading = Modifica { -collection }
form-name-fixed = Il nome è fisso (è l'identità della { -collection }); modifica la descrizione qui sotto.
form-drafted-with = Bozza generata con
form-drafted-at = presso
form-drafted-explain = I campi contrassegnati “bozza” erano vuoti e ora contengono la bozza; dove avevi già scritto qualcosa, la bozza è mostrata sotto, da usare solo se lo scegli. Nulla viene salvato finché non premi Salva.
form-label-name = Nome
form-label-description = Descrizione
form-hint-description = un riepilogo di una riga, mostrato sulle schede e sotto il titolo
form-hint-subjects = separati da virgole
form-label-narrative = Narrazione
form-hint-narrative = il testo completo della descrizione archivistica (Markdown): ambito e contenuto, storia della conservazione, valutazione
form-drafted-mark = bozza, da rivedere prima di salvare
form-draft-suggests = La bozza suggerisce:
form-use-draft = Usa invece la bozza
form-save = Salva le modifiche
form-create = Crea { -collection }
form-draft-again = Genera di nuovo la bozza
form-draft-new = Genera una bozza della descrizione
draft-error-no-model = Nessun modello linguistico è configurato, quindi non c'è nulla con cui generare una bozza. L'operatore ne imposta uno in config.yaml (llm:).
draft-error-gather = Impossibile raccogliere le { -page-plural } di questa { -collection }: { $detail }
draft-error-failed = Non è stato possibile generare la bozza: { $detail }
draft-panel-with = Genera la bozza con
draft-panel-at = presso
draft-panel-sent = a cui viene inviato un campione del testo archiviato di questa { -collection }.
draft-panel-summary = Cosa verrà inviato: { $pages }, { $chars }
draft-panel-pages = { $count ->
    [one] { $shown } { -page }
   *[other] { $shown } { -page-plural }
}
draft-panel-chars = { $count ->
    [one] { $shown } carattere
   *[other] { $shown } caratteri
}
draft-panel-prompt = Il prompt completo

# Il banco dei versamenti, dove si aggiungono scansioni a una raccolta.
desk-eyebrow = Versamento
desk-heading = Aggiungi { -crawl-plural }
desk-collection = Raccolta
desk-collection-placeholder = a quale { -collection } appartiene
desk-name = Nome visualizzato
desk-optional = facoltativo
desk-name-placeholder = sostituisce il nome visualizzato della { -collection }
desk-tab-upload = Carica
desk-tab-url = URL
# Intorno all'estensione `.wacz`, mostrata come codice tra i due messaggi.
desk-upload-before = Carica un file
# Vuoto: in italiano nulla segue l'estensione. Fluent non ammette valori vuoti.
desk-upload-after = { "" }
desk-location = Indirizzo
desk-location-hint = un URL http(s)://
desk-bx-intro = Scegli le { -crawl-plural } da importare nella { -collection } qui sopra, dall'istanza di Browsertrix per cui questo server è configurato (credenziali e host).
desk-bx-org = Organizzazione
desk-bx-collection = Raccolta di Browsertrix
desk-bx-optional-filter = filtro facoltativo
desk-bx-show = Mostra
desk-bx-all = Tutte le { -crawl-plural }
desk-bx-reviewed = Solo con controllo qualità
desk-bx-unreviewed = Senza controllo qualità
desk-hide-imported = Nascondi quelle già importate
desk-refresh = Aggiorna l'elenco
desk-on-import = All'importazione
desk-mode-download = Scarica una copia durevole
desk-mode-download-hint = conservata in locale, riproducibile offline
desk-mode-stream = Lascia in streaming
desk-mode-stream-hint = nessuna copia locale; la riproduzione si risolve di nuovo con le credenziali di questo server
desk-ait-intro = Scegli le { -crawl-plural } da importare nella { -collection } qui sopra, dall'account Archive-It per cui questo server è configurato. Ogni { -crawl } selezionata viene scaricata e impacchettata in un WACZ.
desk-ait-collection = Raccolta di Archive-It
desk-submit = Aggiungi

# Frasi che gli script del workroom mostrano (static/assets/manage.js e editor.js).
# La pagina le consegna come JSON e lo script riempie i segnaposto `{nome}`, quindi
# un argomento si scrive come negli altri messaggi (`{ $label }`) e arriva allo
# script come `{label}`. Un messaggio la cui chiave termina in `-count` è un
# plurale: sceglie su `$count`, mostra `$shown`, e lo script riceve un testo per
# ogni categoria plurale.
js-reading = lettura di { $label }
js-total = record da indicizzare: { $total }
js-indexed-count = { $count ->
    [one] indicizzato { $label } ({ $shown } { -page })
   *[other] indicizzato { $label } ({ $shown } { -page-plural })
}
js-done = Fatto ✓
js-view-crawl = Vai alla { -crawl } →
js-view-collection = Vai alla { -collection } →
js-view-collection-named = Vai alla { -collection } “{ $name }” →
js-added-crawls-count = { $count ->
    [one] Aggiunta { $shown } { -crawl }:
   *[other] Aggiunte { $shown } { -crawl-plural }:
}
js-request-failed = Richiesta non riuscita: { $detail }
js-loading-crawls = Caricamento delle { -crawl-plural }…
js-no-crawls = Nessuna { -crawl } trovata.
js-no-importable-crawls = Nessuna { -crawl } importabile trovata.
js-no-crawls-match = Nessuna { -crawl } corrisponde a questo filtro.
js-qa-done = con controllo qualità
js-qa-done-status = con controllo qualità { $status }/5
js-qa-not-done = senza controllo qualità
js-in-library = già presente
js-crawl-label = { -crawl } { $id }
js-connecting-browsertrix = Connessione a Browsertrix…
js-connecting-archiveit = Connessione ad Archive-It…
js-no-organizations = Nessuna organizzazione visibile con queste credenziali.
js-no-archiveit-collections = Nessuna { -collection } di Archive-It visibile con queste credenziali.
js-name-collection = Indica il nome della { -collection }.
js-choose-file = Scegli un file .wacz da caricare.
js-enter-url = Inserisci un URL http(s)://.
js-select-crawl = Seleziona almeno una { -crawl } da importare.
js-uploading = Caricamento…
js-starting = Avvio…
js-importing = Importazione…
js-source-unavailable = Questa fonte non è ancora disponibile.
js-draft-model-default = il modello
js-drafting = Generazione della bozza…
js-draft-waiting = In attesa di { $service }: { $s } s (rinuncia dopo { $limit } s)
js-all-crawls = Tutte le { -crawl-plural }

# Il banner del visualizzatore e il pannello delle note accanto alla riproduzione
# (static/replay/viewer.js e annotations.js).
replay-title = Riproduzione - { $name }
viewer-notes = Note
viewer-notes-n = Note ({ $total })
viewer-loading = Caricamento della pagina archiviata…
viewer-loading-slow = La pagina archiviata è ancora in caricamento. Le pagine delle { -crawl-plural } di grandi dimensioni possono richiedere tempo.
viewer-frame-title = Pagina archiviata
viewer-captured = Acquisita il { $day } { $month } { $year }
viewer-captured-time = Acquisita il { $day } { $month } { $year }, ore { $time } UTC
anno-close = Chiudi le note
anno-add-page = Aggiungi una nota su questa pagina
anno-add-selection = Aggiungi una nota sulla selezione
anno-empty-can-annotate = Nessuna nota su questa pagina. Seleziona del testo nella pagina o aggiungi una nota sull'intera pagina.
anno-empty = Nessuna nota su questa pagina.
anno-anonymous = anonimo
anno-date-edited = { $date }, modificata
anno-link = Collegamento a questa nota
anno-edit = Modifica
anno-delete = Elimina
anno-save = Salva
anno-add-note = Aggiungi nota
anno-cancel = Annulla
anno-placeholder = Scrivi una nota… (Markdown)
anno-public = Le note sono pubbliche: chiunque possa leggere questo { -archive } può leggerle.
anno-new-passage = Nuova nota su questo brano in { $url }
anno-new-page = Nuova nota su { $url }
anno-confirm-delete = Eliminare questa nota?
anno-failed-add = Impossibile aggiungere la nota ({ $status })
anno-failed-save = Impossibile salvare la nota ({ $status })
anno-failed-delete = Impossibile eliminare la nota ({ $status })
anno-selection-too-long = La selezione è troppo lunga per essere citata (al massimo { $max } caratteri).

annotations-index-title = Annotazioni - { $name }
annotations-index-count = { $count ->
    [one] { $shown } nota sulle pagine di questa { -collection }.
   *[other] { $shown } note sulle pagine di questa { -collection }.
}
annotations-index-empty = Ancora nessuna annotazione.
annotations-open-page = Apri la pagina
annotations-link = Collegamento a questa nota
note-heading = Nota
note-title = Nota - { $name }
note-json-ld = Questa nota come W3C Web Annotation:
replay-not-configured = La riproduzione non è configurata: questo server non ha un'origine di riproduzione.

# Cosa sta facendo l'indicizzazione, mostrato nella pagina di aggiunta mentre il
# lavoro è in corso.
phase-downloading = scaricamento
phase-resolving = risoluzione dell'indirizzo
phase-checksumming = calcolo dell'impronta
# Non "scansione": in indice quella parola indica la { -crawl } (vedi i termini).
phase-scanning = analisi dei record
phase-reading-index = lettura dell'indice
phase-building-index = costruzione dell'indice
phase-committing = salvataggio
phase-skipping-already-indexed = salto di { $location }, già indicizzato
phase-nested-wacz = WACZ annidato { $n }/{ $of }
phase-waiting-for = in attesa che { $holder } termini…
phase-waiting-for-import = in attesa che un'altra importazione termini…
phase-building-wacz = creazione del WACZ
phase-downloading-file = scaricamento di { $name } ({ $size }) [{ $n }/{ $of }]
phase-segments = { $count ->
    [one] { $count } segmento
   *[other] { $count } segmenti
  }
phase-expunging-deletes = rimozione delle eliminazioni ({ $segments ->
    [one] { $segments } segmento
   *[other] { $segments } segmenti
  })
phase-removed-orphans = { $files ->
    [one] rimosso { $files } file orfano
   *[other] rimossi { $files } file orfani
  }
phase-reading-manifest = lettura del manifesto
phase-listing-crawls = elenco delle { -crawl-plural } nell'indice
phase-looking-for-orphan-files = ricerca dei file orfani sul disco
phase-downloading-file-unsized = scaricamento di { $name } [{ $n }/{ $of }]

# The English name of each language indice has a catalog for, whatever the
# language of this catalog: the AI description prompt is written in English and
# names the language the model should answer in (`site.language`). Keep these
# in English.
language-name-en = English
language-name-it = Italian
