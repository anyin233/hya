# What did the Linux watcher improve before it was integrated?

On two non-Git E5 workspaces, the G-fixed kernel observed 30 of 70 staged path changes at the first eligible boundary. The native watcher observed all 70, including creations and equal-size rewrites with restored mtimes. Both builds passed both original tasks. The follow-up stopped before the watcher ran a formal task, leaving its effect on task pass rate unmeasured. The source data and scoring rules are retained in experiment/e5-files at 454e053a.

[Kernel observation contract](../docs/kernel.md#workspace-file-observations)
