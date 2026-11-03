#!/usr/bin/env bash
# Puts the notarized menu bar app on a GitHub release and points the Homebrew cask at it.
#
#   apps/macos/publish.sh v0.1.2
#
# Builds build/Strata-macOS.zip first if it is missing (bundle.sh --release: Developer ID,
# notarized, stapled). The cask is Casks/strata-menubar.rb in C01-Dev/homebrew-tap:
#   brew install --cask c01-dev/tap/strata-menubar
set -euo pipefail
cd "$(dirname "$0")"
tag=${1:?usage: publish.sh <tag, e.g. v0.1.2>}
version=${tag#v}
zip=build/Strata-macOS.zip
[ -f "$zip" ] || ./bundle.sh --release
gh release upload "$tag" "$zip" --clobber --repo C01-Dev/Strata
sha=$(shasum -a 256 "$zip" | cut -d' ' -f1)

cask=$(cat <<EOF
cask "strata-menubar" do
  version "$version"
  sha256 "$sha"

  url "https://github.com/C01-Dev/Strata/releases/download/v#{version}/Strata-macOS.zip"
  name "Strata"
  desc "Menu bar app for Strata: every coding agent's worktree, memory and dev server"
  homepage "https://strata.c01.dev/"

  depends_on arch: :arm64
  depends_on macos: ">= :sonoma"

  app "Strata.app"

  caveats <<~EOS
    Strata.app runs the strata command. Install it too if you have not:
      brew install c01-dev/tap/strata
  EOS
end
EOF
)
path=Casks/strata-menubar.rb
old=$(gh api "repos/C01-Dev/homebrew-tap/contents/$path" --jq .sha 2>/dev/null || true)
gh api -X PUT "repos/C01-Dev/homebrew-tap/contents/$path" \
  -f message="strata-menubar $version" \
  -f content="$(printf '%s\n' "$cask" | base64)" \
  ${old:+-f sha="$old"} --jq '.commit.sha'
echo "published $zip ($sha) and $path"
