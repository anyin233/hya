# 0.36.3

## Session forking

- Forked Sessions now retain the source Session workdir instead of falling back
  to the backend process workdir. Tools, command discovery, Skills, plugins, and
  MCP continue to resolve against the original project after a fork.
