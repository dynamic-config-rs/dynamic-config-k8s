#!/usr/bin/env bash
# Sourced by propose.sh and promote.sh — the one copy of the rule that
# titles a promotion. The version lives in the workspace Cargo.toml.
#
# Sets: $title
promotion_title() {
  git fetch -q origin main

  local version released
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
  released=$(git show origin/main:Cargo.toml | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)

  if [ "$version" != "$released" ]; then
    title="release $version"
  elif ! git show origin/main:CHANGELOG.md 2>/dev/null | grep -q "^## $version "; then
    # The first release does not move the version; the changelog gaining
    # the heading is what marks it.
    title="release $version"
  else
    title="promote dev to main"
  fi
}
