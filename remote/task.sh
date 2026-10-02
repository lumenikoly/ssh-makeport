# A PTY is mandatory: its terminal driver delivers Ctrl+C to the foreground
# process group, including make, the recipe shell and ordinary child processes.
set -u
project=$1
token=$2
target=$3
case "$project" in
    '~') project=$HOME ;;
    '~/'*) project="$HOME/${project#\~/}" ;;
    /*) ;;
    *) exit 64 ;;
esac
CDPATH= cd "$project" || exit 72
stty -echo isig icanon intr '^C' || exit 70
# Catch, rather than ignore, SIGINT. Ignoring it would be inherited by make.
# The trap runs after the foreground command returns, so we can acknowledge exit.
trap ':' INT
printf '\n__MAKEPORT_STARTED_%s__\n' "$token"
export TERM=dumb
export NO_COLOR=1
export CLICOLOR=0
make "$target"
result=$?
printf '\n__MAKEPORT_EXIT_%s__:%s\n' "$token" "$result"
exit "$result"
