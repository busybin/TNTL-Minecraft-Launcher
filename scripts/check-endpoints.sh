#!/usr/bin/env bash
# Every host the code may contact must be on the list kept in docs/02-endpoints.md.
# Unknown hosts fail the build, which is what keeps the source policy honest.
set -euo pipefail

cd "$(dirname "$0")/.."

allowed=(
    mojang.com minecraft.net minecraftservices.com
    microsoftonline.com live.com xboxlive.com learn.microsoft.com
    fabricmc.net legacyfabric.net quiltmc.org
    minecraftforge.net neoforged.net spongepowered.org
    modrinth.com modpacks.ch forgecdn.net curseforge.com
    adoptium.net github.com githubusercontent.com
    maven.org ely.by mineskin.org wiki.vg
    example.com example.invalid example localhost 127.0.0.1
)

status=0

while IFS= read -r hit; do
    host=${hit#*:*:}
    host=${host#*//}
    host=${host%%/*}
    host=${host%%:*}

    # Templates and placeholder hosts in tests have no dot; only real names matter.
    case $host in *.*) ;; *) continue ;; esac

    ok=0
    for suffix in "${allowed[@]}"; do
        case $host in
            "$suffix" | *".$suffix") ok=1; break ;;
        esac
    done
    if [ "$ok" -eq 0 ]; then
        printf '%s\n' "$hit"
        status=1
    fi
done < <(grep -RInoE 'https?://[^"/ ]+' \
    crates apps/desktop/src \
    --include='*.rs' --include='*.ts' --include='*.vue' \
    --exclude-dir=node_modules --exclude-dir=dist 2>/dev/null || true)

if grep -RInE 'GetUserGeoID|country_code|TimeZone\.getDefault' crates apps/desktop/src; then
    echo "region detection found" >&2
    status=1
fi

if [ "$status" -ne 0 ]; then
    echo "endpoint check failed: add the host to docs/02-endpoints.md and to this list" >&2
    exit 1
fi

echo "endpoint check passed"
