#!/bin/sh
# Clone the `rtk fmt` acceptance corpus, and record what it resolved to.
#
# Phase 5 of docs/rtk-fmt-plan.md needs real Grafana Jsonnet, vendor included.
# There is no Grafana-internal checkout available to this repository —
# `grafana/deployment_tools` is private and a gate whose corpus lives on one
# laptop cannot be enforced — so the corpus is public Grafana Jsonnet, pinned.
# fmt-acceptance.toml carries the whole argument and is the one place the
# repository list lives.
#
# Two things this does that a bare `git clone` would not:
#
#   * it writes `<corpus>/<name>.rev` with the commit each clone resolved to.
#     That is the provenance the harness prints and asserts, and it is the only
#     way a shallow clone of a moving branch can answer for a recorded number.
#   * it fails if the repository list comes out empty. A loop over nothing
#     would otherwise leave an empty corpus directory and exit 0, and the next
#     thing to run would be a gate with no corpus.
#
# Usage: scripts/fmt-acceptance-corpus.sh [config] [corpus-dir]

set -eu

config="${1:-fmt-acceptance.toml}"
corpus="${2:-target/fmt-acceptance-corpus}"

if [ ! -f "$config" ]; then
	echo "$config does not exist" >&2
	exit 1
fi

mkdir -p "$corpus"
list="$corpus/repos.tsv"

# A three-field extraction rather than a TOML parser, because the only thing
# needed from the file is the `[[repos]]` triples and the alternative is a
# Python version dependency (`tomllib` is 3.11+) in a script whose whole job is
# to run `git clone`. `rev` is deliberately not read here: the harness compares
# it against what this script records, and a clone that consulted the pin could
# not report drift from it.
awk '
	function value(line) {
		sub(/^[^=]*=[ \t]*"/, "", line)
		sub(/".*$/, "", line)
		return line
	}
	function emit() {
		if (name != "") { printf "%s\t%s\t%s\n", name, url, ref }
		name = ""; url = ""; ref = ""
	}
	/^\[\[repos\]\]/ { emit(); inrepo = 1; next }
	/^\[/           { emit(); inrepo = 0; next }
	inrepo && /^[ \t]*name[ \t]*=/ { name = value($0); next }
	inrepo && /^[ \t]*url[ \t]*=/  { url  = value($0); next }
	inrepo && /^[ \t]*ref[ \t]*=/  { ref  = value($0); next }
	END { emit() }
' "$config" > "$list"

if [ ! -s "$list" ]; then
	echo "no [[repos]] entries found in $config" >&2
	exit 1
fi

echo "cloning the fmt acceptance corpus into $corpus"
while IFS="$(printf '\t')" read -r name url ref; do
	if [ -z "$name" ] || [ -z "$url" ] || [ -z "$ref" ]; then
		echo "incomplete [[repos]] entry: name='$name' url='$url' ref='$ref'" >&2
		exit 1
	fi

	destination="$corpus/$name"
	if [ -d "$destination/.git" ]; then
		echo "  $name: already present, left alone"
	else
		# Shallow and single-branch: the corpus is the working tree, not the
		# history. A ref that does not exist fails here rather than silently
		# falling back to the default branch — a fallback would make the
		# recorded numbers answer for a branch nobody named.
		rm -rf "$destination"
		git clone --quiet --depth 1 --single-branch --branch "$ref" "$url" "$destination"
		echo "  $name: cloned $url @ $ref"
	fi

	git -C "$destination" rev-parse HEAD > "$corpus/$name.rev"
	echo "  $name: $(cat "$corpus/$name.rev")"
done < "$list"

echo
echo "Corpus ready. Run 'make fmt-acceptance'."
echo "The revisions above are what the recorded counts answer for; put them in"
echo "the 'rev' of each [[repos]] entry in $config."
