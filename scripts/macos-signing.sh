#!/usr/bin/env bash
# Sourced by bundle-macos.sh. No keychain credentials are read or exported here.

signing_error() {
  echo "macOS signing: $*" >&2
  return 1
}

# Resolve the deepest existing parent without creating anything. A custom
# distribution directory must never turn output cleanup into daily-app deletion.
validate_distribution_path() {
  local directory=$1 destination=$2 parent suffix='' resolved
  [[ "$BUNDLE_NAME" != */* && "$BUNDLE_NAME" != . && "$BUNDLE_NAME" != .. ]] || signing_error "bundle name must be a name, not a path" || return 1
  destination="$(cd "$(dirname "$destination")" && pwd -P)/$(basename "$destination")" || return 1
  parent=$directory
  while [[ ! -d "$parent" ]]; do
    [[ "$(basename "$parent")" != ".." ]] || signing_error "unresolved '..' in distribution path" || return 1
    suffix="/$(basename "$parent")$suffix"
    parent=$(dirname "$parent")
  done
  resolved=$(cd "$parent" && pwd -P) || return 1
  resolved="$resolved$suffix"
  # macOS treats //Applications as /Applications, but pwd may preserve //;
  # filesystem identity also catches case-insensitive volume aliases.
  while [[ "$resolved" == //* ]]; do resolved=${resolved#/}; done
  if [[ "$resolved" == "$destination" || "$resolved" == "$destination/"* || "$resolved/$BUNDLE_NAME.app" == "$destination" || "$resolved/$BUNDLE_NAME.app" -ef "$destination" ]]; then
    signing_error "distribution output overlaps the installed app"
    return 1
  fi
  while [[ "$parent" != / && "$parent" != . ]]; do
    if [[ "$parent" -ef "$destination" ]]; then
      signing_error "distribution output is inside the installed app"
      return 1
    fi
    parent=$(dirname "$parent")
  done
}

# Pin an explicitly selected, valid identity to its SHA-1 fingerprint. A name
# matching multiple certificates is not a selection; never choose one for it.
resolve_developer_id() {
  local identities matches count name
  [[ -n "$IDENTITY" ]] || signing_error "STENLY_SIGN_IDENTITY is required for --install, release, or notarization builds" || return 1
  identities=$(security find-identity -v -p codesigning) || return 1
  matches=$(printf '%s\n' "$identities" | awk -F '"' -v selected="$IDENTITY" '
    NF >= 3 { split($1, fields, " "); hash=fields[2];
      if ($2 == selected || hash == toupper(selected)) print hash "\t" $2 }')
  count=$(printf '%s\n' "$matches" | awk 'NF {n++} END {print n+0}')
  [[ "$count" == 1 ]] || signing_error "identity must match exactly one valid codesigning certificate (found $count)" || return 1
  SIGN_IDENTITY_HASH=${matches%%$'\t'*}
  name=${matches#*$'\t'}
  [[ "$SIGN_IDENTITY_HASH" =~ ^[A-Fa-f0-9]{40}$ && "$name" =~ ^Developer\ ID\ Application:\ .+\ \(([A-Z0-9]{10})\)$ ]] || signing_error "selected identity is not a Developer ID Application certificate" || return 1
  SIGN_TEAM=${BASH_REMATCH[1]}
  DEVELOPER_REQUIREMENT="anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$SIGN_TEAM\""
}

# Fail closed if the installed bundle is unreadable, ad-hoc, damaged, or belongs
# to another team. An intentional signing migration needs a separate workflow.
read_installed_requirement() {
  local app=$1 metadata team requirement
  INSTALLED_REQUIREMENT=
  [[ -e "$app" || -L "$app" ]] || return 0
  [[ -d "$app" && ! -L "$app" ]] || signing_error "installed app is not a regular bundle directory" || return 1
  codesign --verify --deep --strict "$app" || return 1
  metadata=$(codesign -d --verbose=4 "$app" 2>&1) || return 1
  team=$(printf '%s\n' "$metadata" | sed -n 's/^TeamIdentifier=//p')
  [[ "$team" == "$SIGN_TEAM" ]] || signing_error "installed app team differs from the selected Developer ID; refusing replacement" || return 1
  requirement=$(codesign -d -r- "$app" 2>&1) || return 1
  INSTALLED_REQUIREMENT=$(printf '%s\n' "$requirement" | sed -n 's/^designated => //p')
  [[ -n "$INSTALLED_REQUIREMENT" ]] || signing_error "cannot read installed app designated requirement" || return 1
}

# Prove that the key is usable and is really Apple's Developer ID Application
# certificate type, not just a similarly named certificate. Check the installed
# app's exact designated requirement too, before cargo or output cleanup runs.
probe_signing_identity() (
  local probe_dir
  probe_dir=$(mktemp -d "${TMPDIR:-/tmp}/stenly-signing.XXXXXX") || return 1
  trap 'rm -rf "$probe_dir"' EXIT
  cp /usr/bin/true "$probe_dir/probe" || return 1
  codesign --force "${SIGN_ARGS[@]}" --identifier "$BUNDLE_ID" "$probe_dir/probe" || return 1
  codesign --verify --strict -R="$DEVELOPER_REQUIREMENT" "$probe_dir/probe" || return 1
  if [[ -n "${INSTALLED_REQUIREMENT:-}" ]]; then
    codesign --verify --strict -R="$INSTALLED_REQUIREMENT" "$probe_dir/probe" || return 1
  fi
)

validate_install_candidate() {
  local candidate=$1
  codesign --verify --deep --strict "$candidate" || return 1
  # External requirements constrain the outer app, not differently identified
  # nested helpers. Validate nested integrity separately above.
  codesign --verify --strict -R="$DEVELOPER_REQUIREMENT" "$candidate" || return 1
  if [[ -n "$INSTALLED_REQUIREMENT" ]]; then
    codesign --verify --strict -R="$INSTALLED_REQUIREMENT" "$candidate" || return 1
  fi
}

install_signed_app() (
  local candidate=$1 destination=$2 stage
  # Re-read just before staging, in case another install happened during build.
  read_installed_requirement "$destination" || return 1
  validate_install_candidate "$candidate" || return 1
  stage=$(mktemp -d "$(dirname "$destination")/.stenly-install.XXXXXX") || return 1
  # Never erase the backup if rollback fails. Tell the operator where it is.
  trap 'if [[ -e "$stage/Previous.app" ]]; then echo "Previous app preserved at $stage/Previous.app" >&2; else rm -rf "$stage"; fi' EXIT
  cp -R "$candidate" "$stage/Stenly.app" || return 1
  validate_install_candidate "$stage/Stenly.app" || return 1
  if [[ -e "$destination" ]]; then
    mv "$destination" "$stage/Previous.app" || return 1
  fi
  if ! mv "$stage/Stenly.app" "$destination"; then
    if [[ -e "$stage/Previous.app" ]]; then
      mv "$stage/Previous.app" "$destination" || return 1
    fi
    signing_error "replacement failed; previous app restored (if present)"
    return 1
  fi
  # Only stop the running app after copy, signature/continuity checks, and swap
  # succeeded. Copy failures above leave both the app and process alone.
  pkill -x stenly 2>/dev/null || true
  open -g "$destination" || return 1
  rm -rf "$stage/Previous.app"
  echo "Installed and launched $destination"
)
