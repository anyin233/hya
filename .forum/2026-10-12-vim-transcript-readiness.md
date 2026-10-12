# Why did the Vim terminal test fail after Enter?

The composer is cleared before the server event adds the submitted text to the transcript. CI run 38291351224 passed 306/307 cases but read this intervening frame in the Vim test. Wait for the empty composer and then `waitForText(submittedText)`; neither key delivery nor an empty composer acknowledges transcript rendering. No sleep or retry is needed.
