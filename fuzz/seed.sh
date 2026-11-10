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
