set -eu
mkfifo .isoplane/publication-gate
printf 'publication-is-held\n'
exec cat .isoplane/publication-gate
