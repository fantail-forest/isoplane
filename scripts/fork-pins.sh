# shellcheck shell=bash
# Sourced by check-fork-pins and move-fork-pins: the fork commits a checkout
# pins and where a commit stands on its fork's trunk.
# Needs a signed-in GitHub CLI.
fork_owner=fantail-forest
trunk=isoplane-revisions

# fork_pins [COMMIT] prints "FORK LOCATION REVISION" for each pin of the checkout
# in the current directory, or of that commit in it. LOCATION is a submodule
# path, or Cargo.lock for the Wasmtime pin.
fork_pins() {
  local commit=${1:-HEAD} runtime openssl debugger lockfile wasmtime
  runtime=$(git rev-parse "$commit:vendor/dotnet-runtime")
  openssl=$(git rev-parse "$commit:vendor/openssl")
  debugger=$(git rev-parse "$commit:tools/debugger-libs")
  if [ "$#" = 0 ]; then lockfile=$(cat Cargo.lock); else lockfile=$(git show "$commit:Cargo.lock"); fi
  wasmtime=$(sed -n 's/.*isoplane-wasmtime?rev=\([0-9a-f]\{40\}\).*/isoplane-wasmtime Cargo.lock \1/p' <<<"$lockfile" | sort -u)
  echo "isoplane-dotnet-runtime vendor/dotnet-runtime $runtime"
  echo "isoplane-openssl vendor/openssl $openssl"
  echo "isoplane-debugger-libs tools/debugger-libs $debugger"
  echo "$wasmtime"
}

# trunk_status FORK REVISION prints how the revision compares with the fork's
# trunk. It is on the trunk when that is identical or behind.
trunk_status() {
  gh api "repos/$fork_owner/$1/compare/$trunk...$2" --jq .status
}

# trunk_head FORK prints the commit the fork's trunk is at.
trunk_head() {
  gh api "repos/$fork_owner/$1/git/ref/heads/$trunk" --jq .object.sha
}

# landed_pull_request FORK REVISION prints "NUMBER COMMIT" when the revision is
# the head of exactly one pull request merged into the fork's trunk, where
# COMMIT is what the merge put on the trunk. It prints nothing otherwise.
landed_pull_request() {
  local merged
  merged=$(gh api "repos/$fork_owner/$1/commits/$2/pulls" --paginate --jq "
    .[] | select(.merged_at != null and .base.ref == \"$trunk\" and .head.sha == \"$2\")
    | \"\(.number) \(.merge_commit_sha)\"") || return
  # Two matching pull requests give two lines, which this does not match.
  if [[ $merged =~ ^[0-9]+\ [0-9a-f]{40}$ ]]; then
    echo "$merged"
  fi
}
