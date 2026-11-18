# 0.48.0-dev.6

- On Linux, bare and full kernel rounds track workspace files with a shared native watcher. Newly created files, renamed paths, and writes that preserve size and modification time become visible in session context at the next observation boundary, including in directories without Git.
