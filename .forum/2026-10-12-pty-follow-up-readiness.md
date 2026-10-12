# Why did PR 32's compact and narrow fork terminal tests fail intermittently?

CI run 38266343551 passed all nine PTY checks and 297/299 shared scenarios.
The compact capture showed the preceding turn still busy; `waitForIdle` had
observed the pre-admission frame without a working indicator. Await the actual
reply before checking idle and submitting `/compact`.

The narrow undo capture showed both the pending-revert marker and the original
reply. Revert metadata is applied before the asynchronous transcript read and
input prefill complete, so the subsequent redo wait could match the old reply.
The frozen fork picker then had only its head row, even though the restored
transcript appeared behind it later. Await disappearance of the old reply and
the completed input prefill before redo, then the restored reply and removal of
the marker before fork. No fixed sleeps, retries, exclusions, or weaker outcome
assertions are needed.

The same specifications run on the browser and PTY drivers. PTY input flushes
bytes already received; it does not acknowledge that the child handled a key
or completed a server request.
