# args: blast -d 1 --export-viz vizout
# P1-24, P1-65: an `origin` remote names the page. The repo name is the
# last path segment of the remote URL, without `.git`, not the directory.
git remote add origin https://github.com/acme/sieve-demo.git
