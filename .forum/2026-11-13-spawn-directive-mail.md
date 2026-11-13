# Why is a spawn directive the first parent–child DM mail?

M4-8 registers the optional directive as `MailSent` in the same transaction as
child creation, roster/channels and claim acquisition. The ordinary mail listener
or recovery schedules its first wake; registration does not separately arm it.
The wake coalesces mail and runs user-text hooks before `begin_work`. Parked
Workflow registrations omit mail. Direct-parent DM wakes remain deliverable
under restrictive bundle wake policy.

## Why must outgoing DM copies be excluded from work?

The channel reducer retains the mail in both peers' histories, including the
sender's inbox. Ordinary wakes, lag recovery, report prechecks, the transactional
report gate and unread handoff extraction must ignore `from == handle`, as the
steer path already does. Otherwise a parent can execute its own child's directive
or fail its report gate because it sent work. Do not acknowledge a sender's whole
inbox at send time: that could consume unrelated unread mail.

## Can one initializer retract a committed task batch?

No. Validation, capacity, fencing and commit can reject the batch before it is
installed. Afterwards every committed identity is returned; each episode owns
its initialization error and terminal report. Existing archive/kill requests keep
their terminal reasons. A regression blocks one initializer, lets a sibling fail,
then cancels the blocked initializer and checks the actual `task` receipt still
lists both members.

## Does fresh work still use a registered prompt ID?

No. `ResidentWorkPrompt::Text` admits the first mail wake without reading the
child projection inside the writer transaction. `Fresh` and the initial-prompt
fields are gone. `Resume` remains for admitted work retained by user cancellation:
it reuses an unanswered prompt, while an answered prompt stays immutable and new
work is appended at the tail. An episode ended before its first wake has no child
prompt; its unread directive is in the handoff's Goal and Pending sections.
