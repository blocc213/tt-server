#!/bin/sh
set -eu

# Bind-mounted host directories are commonly created with a different uid.
# Normalize ownership before dropping privileges; contents are never deleted or
# replaced. On a large migrated data tree this is intentionally a startup-time
# cost in exchange for deterministic write access.
if [ ! -d /data ]; then
    mkdir -p /data
fi

chown -R tauritavern:tauritavern /data

exec gosu tauritavern "$@"
