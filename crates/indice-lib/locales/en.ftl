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
# A WACZ file indice has indexed and can replay.
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
# The summary, then the most frequent failing HTTP codes: "404×3, 504×1".
capture-quality-failures = { $summary } — { $codes }
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
