# Executed by sh -c over SSH; never installed on the remote host.
set -eu
project=$1
token=$2
case "$project" in
    '~') project=$HOME ;;
    '~/'*) project="$HOME/${project#\~/}" ;;
    /*) ;;
    *) printf '%s\n' 'MakePort: invalid project path' >&2; exit 64 ;;
esac
CDPATH= cd "$project" || exit 72
command -v make >/dev/null 2>&1 || {
    printf '%s\n' 'MakePort: make is not available in the SSH environment' >&2
    exit 69
}
printf '\n__MAKEPORT_READY_%s__\n' "$token"
# stdin remains open in the desktop application. EOF closes the connection.
exec cat >/dev/null
