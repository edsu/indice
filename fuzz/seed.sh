#!/bin/sh
# Seed each target's corpus from the test fixtures. Run from fuzz/.
set -eu
fixtures=../crates/indice-lib/tests/fixtures
mkdir -p corpus/wacz corpus/warc corpus/warc_slice corpus/wacz_members
cp "$fixtures"/*.wacz corpus/wacz/
for f in "$fixtures"/*.warc.gz; do
    cp "$f" corpus/warc_slice/
    gunzip -c "$f" > "corpus/warc/$(basename "$f" .gz)"
done

# The rest start from a few hand-written inputs.
mkdir -p corpus/search corpus/byte_range corpus/markdown
i=0
for q in 'climate' 'title:"sea levels"' 'climate -crawl:c2' 'year:[1990 TO 2022]' \
    'domain:example.com AND (sea OR mer)' 'collection:climate status:404' 'sea*' '"le climat"~2'; do
    i=$((i + 1)); printf '%s' "$q" > "corpus/search/$i"
done
i=0
for r in 'bytes=0-9' 'bytes=90-' 'bytes=-10' 'bytes=10-5'; do
    i=$((i + 1)); { printf 'd\000\000\000\000\000\000\000'; printf '%s' "$r"; } > "corpus/byte_range/$i"
done
printf '# T\n\n*a* [l](https://x.test "t") <b>x</b>\n\n3. one\n\n```rust\nx\n```\n' > corpus/markdown/1
printf '[x](javascript:alert(1)) [y](&#106;avascript:1) ![i](data:x)\n' > corpus/markdown/2
