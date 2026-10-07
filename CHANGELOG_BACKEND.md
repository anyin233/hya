# 0.45.4

- Prepare first-party native tool policies concurrently and reuse verified native-library cache files across daemon starts.
- Probe new daemons promptly and support cross-process startup trace files.
- Run VCS snapshots outside async RPC workers so Git subprocesses do not stall startup requests.
