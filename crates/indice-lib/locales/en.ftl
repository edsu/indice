# English is the reference catalog: every message and term here must exist in
# every other catalog (the `catalogs_have_the_same_messages` test enforces it).
#
# Terms name the things indice talks about, so every message uses the same word
# for the same thing and a translator decides each word once, here.

# One run of a web crawler, a set of captures made together and stored in a WACZ.
-crawl = crawl
# One archived copy of one URL at one moment.
-capture = capture
# A curated, named group of crawls with a finding aid.
-collection = collection
# A note a curator or reader attaches to a page or a passage of a page.
-annotation = annotation
# A person who looks after a collection and writes its description.
-curator = curator
# The whole web archive this indice instance serves, as its readers see it.
-archive = archive
# An archived web page as indexed for search and replay.
-page = page
# Looking for words in the text of archived pages.
-search = search

# Plural of { -crawl }, for counts and lists.
-crawl-plural = crawls
# Plural of { -collection }, for counts and lists.
-collection-plural = collections
# Plural of { -page }, for counts and lists.
-page-plural = pages
# Plural of { -capture }, for counts and lists.
-capture-plural = captures

# Month names, for dates and the timeline. In languages that write months in
# lower case mid-sentence, give them that way here.
month-1 = January
month-2 = February
month-3 = March
month-4 = April
month-5 = May
month-6 = June
month-7 = July
month-8 = August
month-9 = September
month-10 = October
month-11 = November
month-12 = December

# A date in words, "7 January 2025". Order the three parts as the language does.
date-long = { $day } { $month } { $year }
# A month on the timeline, "May 2021".
month-year = { $month } { $year }
# A span of two dates, "7 January 2025 to 3 August 2026".
date-span = { $start } to { $end }

# The decimal and thousands separators for numbers and sizes. Written as string
# literals so a separator that is a space or punctuation survives: Fluent trims
# whitespace around a bare value.
number-decimal = { "." }
number-group = { "," }

# Units for file sizes, in steps of 1024.
size-b = B
size-kb = KB
size-mb = MB
size-gb = GB
size-tb = TB

# The line under the archive's name on the homepage when the operator has not set
# `site.tagline`. It is rendered as Markdown, so keep it plain text.
site-tagline-default = Web archive search and replay

# The page header, on every page.
# The chip shown to someone who may change the archive.
layout-manage = Manage
# The search button, in the header and on the homepage.
layout-search-button = Search
# Followed by the signed-in person's name.
layout-signed-in-as = signed in as
layout-logout = Log out
layout-login = Log in

# Shared button and link words.
action-replay = Replay
action-edit = Edit

# Placeholder of the header search box when it searches everything.
search-placeholder-all = { search-all-collections }…
# Placeholder of a search box scoped to one collection; `$scope` is its name.
search-placeholder-scoped = Search { $scope }…
# The homepage search box.
home-search-placeholder = Search archived pages…

# The "Search tips" disclosure. The query examples themselves (`year:2021`) are
# query syntax and stay as they are; only the words around them are translated.
# A line is built from several messages around the examples, in this order.
search-tips-summary = Search tips
search-tips-intro = Type words to search page titles, headings, body text, descriptions, keywords, author, and URLs.
# In bold, then "-", an example, and `search-tips-all-words-after`.
search-tips-all-words = All words must match
search-tips-all-words-after = finds pages containing both.
search-tips-phrase = an exact phrase (use quotes)
search-tips-either = either word
search-tips-exclude = has "climate", excludes "policy"
search-tips-group = group with parentheses
search-tips-title = match only in the page title
search-tips-author = match the page author
search-tips-site = a whole site, across subdomains
search-tips-domain = only that exact host
search-tips-collection = only pages in that collection
# Between two examples: `year:2021` or `year:[2020 TO 2023]`.
search-tips-or = or
search-tips-year = filter by crawl year
search-tips-month = filter by crawl month
search-tips-modified = filter by Last-Modified year
# Followed by an example and a closing parenthesis: "only PDFs (or type:html)".
search-tips-type = only PDFs (or
search-tips-lang = only pages in that language
# Followed by an example and a closing parenthesis.
search-tips-status = filter by HTTP status (or
search-tips-boost = rank "climate" matches higher
search-tips-note-intro = Searches are case-insensitive. Title matches rank above body matches.
# Between two examples: `domain:` needs the exact host (e.g. `www.example.com`).
search-tips-note-needs = needs the exact host (e.g.
# After an example and a closing parenthesis: "); to match host words loosely, just type them (e.g. `example`)."
search-tips-note-loose = ); to match host words loosely, just type them (e.g.

# The homepage.
home-browse-year = Browse by year
home-top-sites = Top sites
home-collections = Collections
home-new-collection = + New collection
home-empty-cta = Add a crawl to start your first collection.
home-add-crawl = Add a crawl
home-no-collections = There are no collections yet. Add a crawl with
# On a collection card; `$shown` is the formatted number.
home-card-crawls = { $count ->
    [one] { $shown } { -crawl }
   *[other] { $shown } { -crawl-plural }
}
# Where a collection's WACZ files live.
source-stored-and-streamed = Stored here and streamed
source-streamed = Streamed
source-stored = Stored here

# The search results page.
search-nothing-matched = Nothing matched
# "3 results"; `$count` picks the form and `$shown` is the formatted number.
search-results-count = { $count ->
    [one] { $shown } result
   *[other] { $shown } results
}
# Between the count and the query: "3 results for x".
search-results-for = for
search-page-of = Page { $page } of { $total }
search-filters = Filters:
search-timeline-label = Results by month captured
# A time axis bar read aloud, "May 2021: 3 pages".
timeline-pages-count = { $count ->
    [one] { $shown } { -page }
   *[other] { $shown } { -page-plural }
}
search-all-collections = Search all { -collection-plural }
# `$label` names a filter, `$value` its value.
search-without = Search without { $label }: { $value }
# Names of the search filters, in the Refine sidebar and on the chips of active
# filters. The key ends in the filter's query name (`year:2021` is `filter-year`).
filter-collection = Collection
filter-year = Year
filter-site = Site
filter-type = Type
filter-lang = Language
filter-month = Month
filter-domain = Host
filter-status = Status
filter-modified = Modified
filter-crawl = Crawl
search-try-fewer = Try fewer words, or check the spelling.
search-browse-link = Browse the { -collection-plural } by year or by site
search-badge-collection = Collection
search-badge-note = Note
# After a note's badge; `$author` wrote it.
search-note-by = by { $author }
search-captured-times = { $count ->
    [one] (captured { $shown } time)
   *[other] (captured { $shown } times)
}
# Before a link to the collection a result belongs to.
search-in-collection = in
search-previous = Previous
search-next = Next
search-refine = Refine
search-show-more = Show { $shown } more
# The tooltip on an archived error page's badge.
search-status-title = Archived HTTP { $code } { $reason }
search-status-title-bare = Archived HTTP { $code }
http-status-301 = Moved Permanently
http-status-302 = Found (redirect)
http-status-307 = Redirect
http-status-308 = Redirect
http-status-400 = Bad Request
http-status-401 = Unauthorized
http-status-403 = Forbidden
http-status-404 = Not Found
http-status-410 = Gone
http-status-429 = Too Many Requests
http-status-500 = Internal Server Error
http-status-502 = Bad Gateway
http-status-503 = Service Unavailable
http-status-504 = Gateway Timeout

# Shared button words.
action-delete-permanently = Delete permanently

# Labels of the facts tables on collection and crawl pages.
meta-curator = Curator
meta-crawls = Crawls
meta-size = Size
meta-software = Software
meta-capture-dates = Capture dates
meta-capture-quality = Capture quality
meta-created = Created
meta-created-by = Created by
meta-source = Source
meta-review = Review
meta-browsertrix-item = Browsertrix item
meta-collection = Collection
meta-crawl = Crawl
meta-warc-files = WARC files
meta-operator = Operator
meta-user-agent = User-Agent
meta-robots = Robots
meta-crawl-host = Crawl host
meta-part-of = Part of
meta-conforms-to = Conforms to
meta-keywords = Keywords
meta-license = License
meta-multi-wacz = Multi-WACZ
meta-pages = Pages
meta-wacz-modified = WACZ modified
meta-added-by = Added by
meta-named = Named
meta-sha256 = SHA-256
meta-crawled = Crawled
meta-indexed = Indexed
meta-status = Status
# "3 crawls bundled in one file".
meta-multi-wacz-value = { $count ->
    [one] { $shown } { -crawl } bundled in one file
   *[other] { $shown } { -crawl-plural } bundled in one file
}

# The finding-aid fields a collection can still be missing.
collection-field-scope = Scope & Content
collection-field-creator = Creator
collection-field-access = Access & Use

# Browsertrix quality-assurance ratings, 1 (worst) to 5 (best).
review-5 = Excellent
review-4 = Good
review-3 = Fair
review-2 = Poor
review-1 = Bad
review-other = Reviewed
# `$word` is one of the ratings above.
review-label = { $word } ({ $rating }/5)

# "100 captures, 98% ok"; `$count` picks the form, `$shown` is formatted.
capture-quality = { $count ->
    [one] { $shown } { -capture }, { $pct }% ok
   *[other] { $shown } { -capture-plural }, { $pct }% ok
}
# The summary, then the most frequent failing HTTP codes: "100 captures, 98% ok (404×3, 504×1)".
capture-quality-failures = { $summary } ({ $codes })
# Joins two names in a list: "A and B". A longer list is joined pairwise.
list-and = { $first } and { $last }

# One line under a crawl in a collection's list: "38 pages, by Browsertrix-Crawler".
crawl-summary-pages = { $count ->
    [one] { $shown } { -page }
   *[other] { $shown } { -page-plural }
}
crawl-summary-by = by { $software }

# Which finding-aid fields began as a language model's draft. The names fill
# `$fields` in lower case; `$Fields` is the same list with its first letter in
# capitals, for a message that opens with it. `$service` is empty or " (host)".
draft-field-description = description
draft-field-narrative = narrative
draft-field-dates = dates
draft-field-subjects = subjects
draft-fields-unknown = Some fields
drafted-note = { $Fields } drafted with { $model }{ $service }.
drafted-note-reviewed = { $Fields } drafted with { $model }{ $service }, reviewed by { $reviewer }.

# Where a crawl's WACZ file lives, on its page.
source-describe-file = Stored in this archive: { $path }
source-describe-url = Read from { $url }
source-describe-browsertrix = Browsertrix ({ $host }), file { $file }
source-describe-browsertrix-public = Browsertrix public collection ({ $host }), file { $file }
# Tooltips of the badge beside a crawl's name.
source-badge-remote-title = Hosted remotely: indice streams this at replay time and keeps no copy
source-badge-local-title = Stored in this archive's own folder

# Headings of the scoped facet overview on a collection or crawl page.
facet-top-sites = Top sites
facet-by-year = By year
facet-types = Types
facet-languages = Languages

# The collection page.
collection-replay = Replay collection
collection-edit = Edit collection
collection-add-crawls = + Add crawls
collection-about = About this collection
# `$list` names the missing fields, comma separated.
collection-needed = Still needed: { $list } (the finding-aid minimum).
collection-edit-link = Edit this collection
collection-edit-to-add = to add them.
collection-edit-to-describe = to describe it.
collection-empty-nudge = No finding-aid description yet. Add the essentials a reader needs: who gathered it (Creator), why it was archived (Scope & Content), and who may use it (Access).
# Before a command, `indice collection set "name" …`, then `collection-or-edit`.
collection-add-with = Add with
# Before a file path, which ends the sentence.
collection-or-edit = or edit
collection-annotations-heading = Annotations
collection-annotation-count = { $count ->
    [one] { $shown } page note in this collection.
   *[other] { $shown } page notes in this collection.
}
collection-browse-annotations = Browse annotations
collection-crawls-heading = Crawls
collection-timeline-label = Pages crawled by month
collection-no-crawls = No crawls in this collection.
crawl-file-missing = The file is missing
collection-delete-summary = Delete this collection
collection-delete-empty-note = Removes this empty collection's finding aid. This can't be undone.
collection-delete-note = { $count ->
    [one] This collection has { $shown } { -crawl }. Deleting the grouping alone is refused; tick the box to delete its { -crawl } too (its pages, WACZ file, and thumbnail). This can't be undone.
   *[other] This collection has { $shown } { -crawl-plural }. Deleting the grouping alone is refused; tick the box to delete its { -crawl-plural } too (their pages, WACZ files, and thumbnails). This can't be undone.
}
collection-delete-also = { $count ->
    [one] also delete the member { -crawl }
   *[other] also delete all { $shown } member { -crawl-plural }
}

# The crawl page.
# What a crawl named only by its tool's identifier is called: `$date` is a
# date in words, `$time` the hour and minute (17:14). Capitalised, so the
# word for a crawl is written out here rather than taken from the term.
crawl-name-dated = Crawl of { $date }
crawl-name-dated-time = Crawl of { $date }, { $time }
crawl-curator-note = Curator's note
crawl-provenance = Provenance
crawl-file = File
crawl-status-present = ✓ present
crawl-status-missing = ✗ missing
crawl-pages-heading = Pages
crawl-no-pages = No pages are listed in this crawl.
crawl-delete-summary = Delete this crawl
crawl-delete-note = Permanently removes this crawl: its pages from search, the local WACZ file, and its thumbnail. This can't be undone.
# Labels of the finding-aid fields a curator fills in, beside their values.
collection-field-dates = Dates
collection-field-rights = Rights
collection-field-subjects = Subjects

# Shared words of the workroom.
crumb-home = Home
action-cancel = Cancel

# Sentences the server says when a request fails. The text of an underlying
# error (`$detail`) is indice's own English and is passed through unchanged.
error-server = Something went wrong on the server
# `$message` is a sentence; `$id` is a code to quote to whoever reads the log.
error-reference = { $message } (reference { $id }).
error-no-reference = { $message }.
error-request-failed = The request failed: { $detail }
error-query-too-long = search query too long: { $bytes } bytes, limit { $limit }
# `$holder` names what holds the lock, in indice's own English ("a workroom add").
error-index-busy = The search index is busy: { $holder } is running. Your change was not saved; try again shortly.
error-busy = indice is busy; try again in a moment
# `$secs` is how long to wait, in seconds.
error-too-many-requests = too many requests from this address; try again in { $secs }s
error-too-many-streams = too many open archive streams from this address; try again in { $secs }s
error-collection-required = collection is required
# `indice index` is a command and stays as it is.
error-location-not-url = add an archive by an http(s) URL, or upload the file. A path on the server's own disk is not accepted here; use `indice index` on the machine itself for that.
error-file-required = a file is required
error-org-required = org is required
error-select-crawl = select at least one { -crawl }
error-text-field-long = a text field in this upload is longer than { $max } bytes
error-text-field-utf8 = a text field is not valid UTF-8
error-upload-refused = upload refused: { $detail }
error-job-unknown = unknown or already-consumed job
error-collection-unknown = unknown { -collection }
error-collection-name-required = { -collection } name is required
error-collection-not-found = { -collection } not found
error-crawl-not-found = { -crawl } not found
error-annotation-not-found = { -annotation } not found
error-replay-link = This is not a link to an archived page
error-note-empty = note is empty
error-note-target-required = url and timestamp are required
error-annotation-none = no such { -annotation }
error-annotation-not-yours = not your { -annotation }
error-browsertrix-unconfigured = Browsertrix import is not configured: set BROWSERTRIX_TOKEN (or BROWSERTRIX_USER + BROWSERTRIX_PASSWORD) in the server's environment.
error-archiveit-unconfigured = Archive-It import is not configured: set ARCHIVEIT_USER + ARCHIVEIT_PASSWORD in the server's environment.
error-browsertrix = Browsertrix error: { $detail }
error-archiveit = Archive-It error: { $detail }
error-browsertrix-resolver = Browsertrix streaming import requires the resolver (credentials).
error-reload-failed = indexed, but reloading the searcher failed: { $detail }
error-lock-failed = could not lock the search index: { $detail }
# A job (an add or an import) failed; `$detail` is the underlying error as reported.
error-job-failed = the job failed: { $detail }
# `$count` crawls are still in the collection; `$shown` is the formatted number.
error-collection-not-empty = { $count ->
    [one] This { -collection } has { $shown } { -crawl }; tick the box to delete it too, or delete or move it first.
   *[other] This { -collection } has { $shown } { -crawl-plural }; tick the box to delete them too, or delete or move them first.
}

# Why a signed-in person may not do something. The `denied-need-` messages
# complete "your account is not authorized for ...": give them a form that does.
denied-heading = You can't change this archive
denied-title = Not authorized
denied-back = Back to the archive
denied-page-insufficient = Your account isn't authorized for { $what }.
denied-insufficient = forbidden: your account is not authorized for { $what }
denied-unauthenticated = forbidden: this action requires signing in via the front proxy
denied-stale = forbidden: your sign-in has lapsed; reload the page to log in again
denied-need-curating = curating this archive
denied-need-administering = administering this archive
denied-need-delete-others-crawl = deleting a { -crawl } someone else added

# Why an accession was refused. `$cap`, `$used` and `$left` are sizes.
refused-too-large = this is larger than the { $cap } limit on a single upload (uploads.max_mb in config.yaml)
refused-archive-full = the archive is limited to { $cap } and holds { $used }, so there is { $left } left (uploads.max_total_gb in config.yaml)
refused-measure = could not measure the archive: { $detail }

# The line on the accession desk about how much room there is. The sentences
# for each limit are joined by `room-both` when both apply.
room-per-upload = Uploads up to { $cap } each.
room-full = The archive is full ({ $used } of its { $cap } limit), so uploads and download imports are refused until a { -crawl } is removed or the limit is raised. Adding by URL still works.
room-left = The archive holds { $used } of its { $cap } limit; { $left } left.
room-both = { $upload } { $archive }

# The finding-aid form.
form-new-heading = New { -collection }
form-eyebrow = Finding aid
form-edit-heading = Edit { -collection }
form-name-fixed = The name is fixed (it's the { -collection }'s identity); edit the description below.
# Before the model's name, then `form-drafted-at` and the service's name.
form-drafted-with = Drafted with
form-drafted-at = at
form-drafted-explain = Fields marked “drafted” were empty and now hold the draft; where you had already written something, the draft is shown below it, to use only if you choose. Nothing is saved until you press Save.
form-label-name = Name
form-label-description = Description
form-hint-description = a one-line summary, shown on cards and under the title
form-hint-subjects = comma-separated
form-label-narrative = Narrative
form-hint-narrative = the full finding-aid prose (Markdown): Scope & Content, custodial history, appraisal
form-drafted-mark = drafted, review before saving
form-draft-suggests = The draft suggests:
form-use-draft = Use the draft instead
form-save = Save changes
form-create = Create { -collection }
form-draft-again = Draft again
form-draft-new = Draft a description
draft-error-no-model = No language model is configured, so there is nothing to draft with. An operator sets one in config.yaml (llm:).
draft-error-gather = Couldn't gather this { -collection }'s { -page-plural }: { $detail }
draft-error-failed = The draft couldn't be made: { $detail }
# The panel under the Draft button: "Drafts with <model> at <service>, which ...".
draft-panel-with = Drafts with
draft-panel-at = at
draft-panel-sent = which is sent a sample of this { -collection }'s archived text.
# `$pages` and `$chars` are the counts below, already written out.
draft-panel-summary = What will be sent: { $pages }, { $chars }
draft-panel-pages = { $count ->
    [one] { $shown } { -page }
   *[other] { $shown } { -page-plural }
}
draft-panel-chars = { $count ->
    [one] { $shown } character
   *[other] { $shown } characters
}
draft-panel-prompt = The full prompt

# The accession desk, where crawls are added to a collection.
desk-eyebrow = Accession
desk-heading = Add { -crawl-plural }
# The label of the collection box, capitalized like the other labels.
desk-collection = Collection
desk-collection-placeholder = which { -collection } this belongs to
desk-name = Display name
desk-optional = optional
desk-name-placeholder = override the { -collection }'s display name
desk-tab-upload = Upload
desk-tab-url = URL
# Around the `.wacz` file extension, which is shown as code between them.
desk-upload-before = Upload a
desk-upload-after = file
desk-location = Location
desk-location-hint = an http(s):// URL
desk-bx-intro = Pick { -crawl-plural } to import into the { -collection } above, from the Browsertrix instance this server is configured for (its credentials + host).
desk-bx-org = Organization
desk-bx-collection = Browsertrix collection
desk-bx-optional-filter = optional filter
desk-bx-show = Show
desk-bx-all = All { -crawl-plural }
desk-bx-reviewed = QA’d only
desk-bx-unreviewed = Not QA’d
desk-hide-imported = Hide already-imported
desk-refresh = Refresh list
desk-on-import = On import
desk-mode-download = Download a durable copy
desk-mode-download-hint = stored locally, replays offline
desk-mode-stream = Stream in place
desk-mode-stream-hint = no local copy; replay re-resolves via this server's credentials
desk-ait-intro = Pick { -crawl-plural } to import into the { -collection } above, from the Archive-It account this server is configured for. Each selected { -crawl } is downloaded and packaged into a WACZ.
desk-ait-collection = Archive-It collection
desk-submit = Add

# Sentences the workroom's scripts show (static/assets/manage.js and editor.js).
# The page hands them over as JSON and the script fills the `{name}` placeholders,
# so an argument here is written like any other (`{ $label }`) and reaches the
# script as `{label}`. A message whose key ends in `-count` is a plural: it selects
# on `$count` and shows `$shown`, and the script gets one text per plural category.
js-reading = reading { $label }
js-total = records to index: { $total }
js-indexed-count = { $count ->
    [one] indexed { $label } ({ $shown } { -page })
   *[other] indexed { $label } ({ $shown } { -page-plural })
}
js-done = Done ✓
js-view-crawl = View { -crawl } →
js-view-collection = View { -collection } →
js-view-collection-named = View “{ $name }” →
js-added-crawls-count = { $count ->
    [one] Added { $shown } { -crawl }:
   *[other] Added { $shown } { -crawl-plural }:
}
js-error = Error: { $detail }
js-request-failed = Request failed: { $detail }
js-loading-crawls = Loading { -crawl-plural }…
js-no-crawls = No { -crawl-plural } found.
js-no-importable-crawls = No importable { -crawl-plural } found.
js-no-crawls-match = No { -crawl-plural } match this filter.
js-qa-done = QA’d
js-qa-done-status = QA’d { $status }/5
js-qa-not-done = not QA’d
js-in-library = in library
js-crawl-label = { -crawl } { $id }
js-connecting-browsertrix = Connecting to Browsertrix…
js-connecting-archiveit = Connecting to Archive-It…
js-no-organizations = No organizations visible for these credentials.
js-no-archiveit-collections = No Archive-It { -collection-plural } visible for these credentials.
js-name-collection = Please name the { -collection }.
js-choose-file = Choose a .wacz file to upload.
js-enter-url = Enter an http(s):// URL.
js-select-crawl = Select at least one { -crawl } to import.
js-uploading = Uploading…
js-starting = Starting…
js-importing = Importing…
js-source-unavailable = That source isn’t available yet.
js-draft-model-default = the model
js-drafting = Drafting…
js-draft-waiting = Waiting for { $service }: { $s }s (it gives up after { $limit }s)
js-all-crawls = All { -crawl-plural }

# The replay viewer's banner and the notes panel beside the replay
# (static/replay/viewer.js and annotations.js). The page hands them over as JSON,
# like the `js-` messages above, and the script fills the `{name}` placeholders.
replay-title = Replay - { $name }
viewer-notes = Notes
viewer-notes-n = Notes ({ $total })
viewer-loading = Loading the archived page…
viewer-loading-slow = Still loading the archived page. Pages from large { -crawl-plural } can take a while.
viewer-frame-title = Archived page
# The month is its name from the `month-` messages, so a language orders the date as it likes.
viewer-captured = Captured { $day } { $month } { $year }
viewer-captured-time = Captured { $day } { $month } { $year }, { $time } UTC
anno-close = Close notes
anno-add-page = Add a note about this page
anno-add-selection = Add note on selection
anno-empty-can-annotate = No notes on this page yet. Select text in the page, or add a note about the whole page.
anno-empty = No notes on this page.
anno-anonymous = anonymous
anno-date-edited = { $date }, edited
anno-link = Link to this note
anno-edit = Edit
anno-delete = Delete
anno-save = Save
anno-add-note = Add note
anno-cancel = Cancel
anno-placeholder = Write a note… (Markdown)
anno-public = Notes are public, so anyone who can read this { -archive } can read them.
# `{ $url }` stands for the page's address, which the script shows in its own element.
anno-new-passage = New note on this passage at { $url }
anno-new-page = New note about { $url }
anno-confirm-delete = Delete this note?
anno-failed-add = Could not add note ({ $status })
anno-failed-save = Could not save note ({ $status })
anno-failed-delete = Could not delete note ({ $status })
anno-selection-too-long = The selection is too long to quote (at most { $max } characters).

# The Annotations page of a collection and a note's own page.
annotations-index-title = Annotations - { $name }
annotations-index-count = { $count ->
    [one] { $shown } note on pages in this { -collection }.
   *[other] { $shown } notes on pages in this { -collection }.
}
annotations-index-empty = No annotations yet.
annotations-open-page = Open page
annotations-link = Link to this note
note-heading = Note
note-title = Note - { $name }
note-json-ld = This note as a W3C Web Annotation:
replay-not-configured = Replay is not configured: this server has no replay origin.

# What indexing is doing right now, shown on the add page while a job runs.
# The command line shows the English text, so these stay short.
phase-downloading = downloading
phase-resolving = resolving
phase-checksumming = checksumming
phase-scanning = scanning
phase-reading-index = reading index
phase-building-index = building index
phase-committing = committing
# `$location` is the URL or path of a source already in the collection.
phase-skipping-already-indexed = skipping already-indexed { $location }
# The `$n`th of `$of` WACZ files nested inside one.
phase-nested-wacz = nested WACZ { $n }/{ $of }
# `$holder` names the process or operation holding the lock, as it described itself.
phase-waiting-for = waiting for { $holder } to finish…
phase-waiting-for-import = waiting for another import to finish…
phase-building-wacz = building WACZ
# `$size` is the formatted file size; the file is the `$n`th of `$of`.
phase-downloading-file = downloading { $name } ({ $size }) [{ $n }/{ $of }]
phase-segments = { $count } segments
phase-expunging-deletes = expunging deletes ({ $segments } segment(s))
phase-removed-orphans = removed { $files } orphaned file(s)
phase-reading-manifest = reading the manifest
phase-listing-crawls = listing { -crawl-plural } in the index
phase-looking-for-orphan-files = looking for the orphans' files on disk
# As `phase-downloading-file`, when the source does not report the file's size.
phase-downloading-file-unsized = downloading { $name } [{ $n }/{ $of }]

# The English name of each language indice has a catalog for, whatever the
# language of this catalog: the AI description prompt is written in English and
# names the language the model should answer in (`site.language`).
language-name-en = English
language-name-it = Italian
