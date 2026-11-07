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
