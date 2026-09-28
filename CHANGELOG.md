# 0.37.11

## OpenTUI turn heartbeat

Active turns now show an animated row with elapsed time and the time since the
last backend event. After 15 seconds of silence, the row reports that no update
has arrived and offers `/cancel`. The frontend checks the existing `Turn.WaitTurn`
status to stop the indicator on success, failure, or cancellation and surfaces
terminal errors in the status row.
