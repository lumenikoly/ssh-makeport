set -eu
project=$1
case "$project" in
    '~') project=$HOME ;;
    '~/'*) project="$HOME/${project#\~/}" ;;
    /*) ;;
    *) exit 64 ;;
esac
CDPATH= cd "$project" || exit 72
# Absence is different from an unreadable or invalid configuration.
if [ ! -e makeport.toml ]; then exit 3; fi
if [ ! -f makeport.toml ] || [ ! -r makeport.toml ]; then
    printf '%s\n' 'MakePort: makeport.toml is not a readable file' >&2
    exit 66
fi
exec head -c 65537 makeport.toml
