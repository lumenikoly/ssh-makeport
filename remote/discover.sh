set -eu
project=$1
case "$project" in
    '~') project=$HOME ;;
    '~/'*) project="$HOME/${project#\~/}" ;;
    /*) ;;
    *) exit 64 ;;
esac
CDPATH= cd "$project" || exit 72
# GNU Make's default filename precedence. Includes are not evaluated.
for file in GNUmakefile makefile Makefile; do
    if [ -f "$file" ] && [ -r "$file" ]; then
        # One byte beyond the accepted limit detects truncation on the client.
        exec head -c 262145 "$file"
    fi
done
printf '%s\n' 'MakePort: no readable Makefile; add target names manually' >&2
exit 66
