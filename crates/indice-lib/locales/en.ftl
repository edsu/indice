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
