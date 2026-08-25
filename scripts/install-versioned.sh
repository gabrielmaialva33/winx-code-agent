#!/usr/bin/env bash
set -euo pipefail

# Build all cooperating executables together, smoke-test the adapter, then
# atomically move only symlinks. Already-running processes keep their original
# inode; new adapters/daemons can never observe a half-written bundle.

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cargo_home=${CARGO_HOME:-"$HOME/.cargo"}
bin_dir=${WINX_INSTALL_BIN_DIR:-"$cargo_home/bin"}
versions_root=${WINX_INSTALL_VERSIONS_DIR:-"$cargo_home/winx/versions"}
stage_root=$(mktemp -d "${TMPDIR:-/tmp}/winx-install.XXXXXXXX")

cleanup() {
    rm -rf -- "$stage_root"
}
trap cleanup EXIT

cargo build \
    --manifest-path "$repo_root/Cargo.toml" \
    --profile dist \
    --locked \
    --bins \
    --target-dir "$stage_root/target"

artifact_dir="$stage_root/target/dist"
identity=$(
    "$artifact_dir/winx-code-agent" --version \
        | awk 'NF >= 2 { print $2; exit }' \
        | tr -cd 'A-Za-z0-9._+-'
)
if [[ -z "$identity" ]]; then
    echo "failed to read build identity from staged winx-code-agent" >&2
    exit 1
fi

bundle="$versions_root/$identity"
bundle_stage="$versions_root/.${identity}.new.$$"
mkdir -p -- "$versions_root" "$bin_dir"
if [[ ! -d "$bundle" ]]; then
    mkdir -p -- "$bundle_stage/bin"
    for name in winx-code-agent winxd winx-guardian; do
        install -m 0755 -- "$artifact_dir/$name" "$bundle_stage/bin/$name"
    done
    "$bundle_stage/bin/winx-code-agent" --version >/dev/null
    mv -- "$bundle_stage" "$bundle"
fi

for name in winx-code-agent winxd winx-guardian; do
    next_link="$bin_dir/.${name}.new.$$"
    ln -s -- "$bundle/bin/$name" "$next_link"
    mv -Tf -- "$next_link" "$bin_dir/$name"
done

printf 'Installed Winx %s atomically in %s\n' "$identity" "$bundle"
printf "Existing processes were not interrupted; run 'winx-code-agent doctor' to inspect runtime builds.\n"
