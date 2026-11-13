# Composer-to-message transition

Accepted native text sends carry the composer's rendered text upward from the
input field into the actual outgoing bubble. The source text crossfades into the
bubble as its size, wrapping and position settle. The input hint is hidden during
the text handoff, so it never appears underneath the departing text. Sending and
continued typing proceed immediately; transport/encryption never wait for motion.

The reference is mobile `org/telegram/ui/TextMessageEnterTransition.java` and
`ChatListItemAnimator` (GPL-3.0-or-later): a 250ms transition, easing and crossfade
when source and destination layouts differ. Desktop uses native frame-clock
updates and immutable GSK snapshots, so motion does not reflow message text on
each frame. A presentation wrapper reserves the bubble's final space and prevents
a visible destination flash before the handoff.

Animation applies only to newly accepted composer text, never historical rows,
forwarded text or unrelated refreshes. There are at most four simultaneous
transitions; source capture is bounded to ten display lines /4096 characters,
matching mobile's short-message transition policy. Pending destinations time out
and reveal normally. Reduced-motion settings skip the transition. Chat/account
changes, window resizing and hidden destinations restore the ordinary bubble and
clear retained snapshots. The history cache and scroll anchoring remain in use.

Private native verification exercises the real durable text queue, records seven
intermediate frames, measures frame cadence, and checks rapid sends, reduced
motion and switching chats. It sends no real account messages. The GTK/Cairo check
records15 intermediate frames with a16ms maximum gap for the250ms flight.
