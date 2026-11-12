#!/bin/sh
# Run ZAP against the stack in this directory, once per role, each on a fresh
# archive. See README.md.
#
#   security/zap/scan.sh baseline    # passive: spider, then look (minutes)
#   security/zap/scan.sh full        # active: also attacks every request found
#
# Reports go to security/zap/reports/<mode>-<role>.{html,json,md}, with
# indice's log beside them as <mode>-<role>-indice.log.
#
# The image is built once, from this checkout, before the first role, so every
# role scans the same code. BUILD=0 skips that and scans whatever indice:zap
# already is, such as an image built from a release tag.
set -eu

mode=${1:-baseline}
case $mode in
	baseline) script=zap-baseline.py ;;
	full) script=zap-full-scan.py ;;
	*) echo "usage: $0 [baseline|full]" >&2; exit 2 ;;
esac
roles=${ROLES:-anonymous reader curator admin}

dir=$(cd "$(dirname "$0")" && pwd)
compose="docker compose -f $dir/compose.yaml"
mkdir -p "$dir/reports"
if [ "${BUILD:-1}" != 0 ]; then
	$compose build seed
fi

for role in $roles; do
	echo "== $mode as $role"
	# A fresh archive: an active scan as a curator or admin changes it.
	$compose down -v --remove-orphans >/dev/null 2>&1 || true
	$compose up -d --wait

	# ZAP's replacer adds the role header to every request it sends. Anonymous
	# sends none.
	role_config=""
	if [ "$role" != anonymous ]; then
		role_config="-config replacer.full_list(0).description=role \
			-config replacer.full_list(0).enabled=true \
			-config replacer.full_list(0).matchtype=REQ_HEADER \
			-config replacer.full_list(0).matchstr=X-Zap-Role \
			-config replacer.full_list(0).regex=false \
			-config replacer.full_list(0).replacement=$role"
	fi

	status=0
	$compose run --rm zap $script -t http://site/ -c rules.conf -j -m 5 \
		${ZAP_ARGS:-} \
		-r "reports/$mode-$role.html" -J "reports/$mode-$role.json" \
		-w "reports/$mode-$role.md" \
		-z "$role_config" || status=$?
	# indice's log says why a request failed, which ZAP's report does not.
	$compose logs --no-color indice >"$dir/reports/$mode-$role-indice.log" 2>&1
	# ZAP exits 1 or 2 when it reports warnings or failures, which is the
	# result. Anything else means the scan itself failed, and its reports are
	# missing or partial, so the run stops rather than going on to the next role.
	case $status in
	0) ;;
	1 | 2) echo "   ZAP exited $status as $role" ;;
	*)
		echo "ZAP failed as $role (exit $status); its output is above" >&2
		$compose down -v --remove-orphans >/dev/null 2>&1
		exit "$status"
		;;
	esac
done

$compose down -v --remove-orphans >/dev/null 2>&1
