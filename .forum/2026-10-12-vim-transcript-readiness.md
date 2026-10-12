# Why did the Vim terminal test fail after Enter?

The composer is cleared before the server event adds the submitted text to the transcript. CI run 38291351224 passed 306/307 cases but read this intervening frame in the Vim test. Wait for the empty composer and then `waitForText(submittedText)`; neither key delivery nor an empty composer acknowledges transcript rendering. No sleep or retry is needed.

## Why did eight-worker stress expose the archive toggle scenario?

The initial composer can render before startup chooses a session. Creating the archived fixture at that point lets startup resume it (and clear its archived flag). Wait for `statusSessionId(term)` before creating another client's session. The captured failing row had the current-session marker on Shelved work; bootstrap explicitly resumes the newest project chat.

## Why did Escape fail to close the empty Diff view under load?

The empty placeholder appears during loading too, when Escape means cancel. The test now waits for the `Esc close` hint, matching the other Diff scenarios. Eight-worker stress exposed this separate readiness race; no timeout increase is needed.

## Why did undo leave a revert marker after a new reply?

The second undo in the scenario waited only for file deletion. Its UI response could arrive after the next prompt. Wait for removed old reply, revert marker and prompt prefill before clearing and submitting replacement text. Also wait for the initial turn to become idle before undo.
