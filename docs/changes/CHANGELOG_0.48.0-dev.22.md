# 0.48.0-dev.22

- **Bare `bash`: Python output reaches the output file as it is printed.** Commands run with `PYTHONUNBUFFERED=1`. Before, a Python program that outlived the call's wait showed nothing in the file the `[still running: …]` line names until its buffer filled or it exited.
- **Bare `read`: a text file reaches the model as its text.** The result is `{"output": <text>}`; before, it was `{"content": …}`, which the model received as one JSON-escaped string.
- **Workflows: a concurrent stage no longer deadlocks on the store.** Concurrent stages run as their own tasks, so a stage cannot wait for a store connection that its caller holds; this removes the intermittent `sqlite: pool timed out` of workflow runs.
- **Bundle registry: opening waits as long as the session store does** for a busy database, instead of the pool's default 30 seconds.
