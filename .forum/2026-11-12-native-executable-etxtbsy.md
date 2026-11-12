# Why can a freshly materialized native bundle fail with ETXTBSY after its writer closes?

An unrelated fork can inherit the writable file description before the parent
closes it. `O_CLOEXEC` closes that descriptor only at the child's exec. The
parent can therefore close its writer and still get `ETXTBSY` executing the
same inode until the unrelated child reaches exec. A synchronized pipe
handshake reproduced exactly that sequence without sleeps; releasing the
unrelated child made the same file run successfully.

The production path is `hya-app/src/bundle_runtime.rs`: `prepare_source`
materializes native process/MCP resources, then launches them. Its former
`fs::write` followed by chmod had no rename or writer-inheritance barrier.
Stress through the real `materialize` function, with four unrelated `/bin/true`
spawners and a test-only 2 ms pre-exec delay, produced 61/5,000 and 51/5,000
ETXTBSY failures. The delay widens inheritance; production has no delay.

Native resources now stream through stdin to POSIX `tee` (required on the
backend's `PATH`). Only that short-lived child opens the destination for
writing; materialization waits for its exit before chmod and publication.
Concurrent backend spawns cannot inherit a descriptor it never owned. There
are no launch retries or sleeps. Executable paths are classified before writing
so a text declaration sharing a native path cannot reintroduce a parent writer.
The existing native execution unit test includes this shared-path case.

A flock handoff alone is insufficient: Linux
[`__fput`](https://github.com/torvalds/linux/blob/master/fs/file_table.c)
removes file locks before dropping inode write access. Renaming does not change
the inherited writer's inode either.

Source inspection found no other affected runtime materializer: activation
staging produces JavaScript read by Bun, package staging produces archives,
and native tool-family extraction produces libraries consumed by dlopen.
The updater already pipes its script to sh and waits for its writer/extractor
subprocesses. Release rehearsal is a synchronous single-threaded developer
command; its copies precede the smoke checks.

The temporary stress was removed from shipped source. Final production logic
completed two 5,000-launch stress runs with zero ETXTBSY failures; the full
bundle-runtime module also passed while the stress ran alongside it.
Final acceptance passed all 200 default-parallel `hya-app --lib` binary runs
(228 tests per run; 45,600 test executions). Full `hya-app` tests, strict app
clippy, formatting, local `hya` build, and the five `p27_bundle_process` Track P
scenarios also passed.
