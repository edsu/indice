#!/usr/bin/env bash
# Install a version-matched Chrome for Testing + chromedriver for the headless
# browser tests (crates/rustyweb-lib/tests/browser.rs and browser_collection.rs,
# both #[ignore]d).
#
# Why this script exists: those tests drive a real browser via WebDriver, and
# chromedriver refuses to drive a Chrome whose major version doesn't match it.
# Homebrew's chromedriver cask is deprecated, and pairing it with your everyday
# (auto-updating) Chrome tends to drift out of sync. @puppeteer/browsers installs
# a MATCHED pair of "Chrome for Testing" + chromedriver, kept separate from your
# system Chrome. The binaries land in ./chrome and ./chromedriver (gitignored).
#
# Usage:
#   ./scripts/fetch-browser.sh            # install the current stable pair
#   ./scripts/fetch-browser.sh 150.0.7871.187   # a specific version/channel
#
# It prints the exact commands to start chromedriver and run the browser tests.
#
# Requires: npx (Node.js).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CHANNEL="${1:-stable}"

echo "Installing matched Chrome for Testing + chromedriver (${CHANNEL}) via @puppeteer/browsers..."
echo "(into ${ROOT}/chrome and ${ROOT}/chromedriver — both gitignored)"
echo

# Each install prints a final line of the form "<name>@<version> <path>". The
# path may contain spaces (e.g. "Google Chrome for Testing.app"), so take
# everything after the first space.
chrome_line="$(npx -y @puppeteer/browsers install "chrome@${CHANNEL}" | tail -1)"
driver_line="$(npx -y @puppeteer/browsers install "chromedriver@${CHANNEL}" | tail -1)"
chrome_bin="${chrome_line#* }"
chromedriver_bin="${driver_line#* }"

if [[ ! -x "$chrome_bin" || ! -x "$chromedriver_bin" ]]; then
  echo "error: could not locate the installed binaries" >&2
  echo "  chrome:       $chrome_bin" >&2
  echo "  chromedriver: $chromedriver_bin" >&2
  exit 1
fi

cat <<EOF

Installed:
  chrome:       ${chrome_bin}
  chromedriver: ${chromedriver_bin}

Run the headless browser tests:

  # 1) start chromedriver (leave it running, e.g. in another terminal):
  "${chromedriver_bin}" --port=9515

  # 2) point the tests at that Chrome + WebDriver and run the #[ignore]d tests:
  CHROME_BIN="${chrome_bin}" \\
  WEBDRIVER_URL=http://localhost:9515 \\
    cargo test -p rustyweb-lib --test browser_collection -- --ignored

  # (browser.rs is the single-WACZ smoke test; same env, --test browser)
EOF
