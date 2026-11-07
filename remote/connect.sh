# Executed by sh -c over SSH; never installed on the remote host.
set -eu
token=$2
printf '\n__MAKEPORT_READY_%s__\n' "$token"
# stdin remains open in the desktop application. EOF closes the connection.
exec cat >/dev/null
