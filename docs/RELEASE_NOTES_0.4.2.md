# Kumiho Desktop 0.4.2

## The app opens again after its window is closed

On macOS, launching Desktop could do nothing at all: the Dock icon bounced,
nothing appeared, and no crash was reported. An instance was still running with
a window that could not be raised, and `single_instance` made every later
launch exit cleanly against it — so the app stayed unopenable until the process
was killed by hand.

- A second launch now unminimizes and shows the window before focusing it.
  Focus alone does not raise a window that is minimized, hidden, or ordered
  out, which is what left the running instance stranded.
- If the window is genuinely gone while the process lives on, Desktop rebuilds
  it from the same declaration it starts with instead of doing nothing.
- Clicking the Dock icon takes the same path, so macOS's own way back into an
  app works too.

An instance already stuck in this state is running code that predates the fix,
so it still needs one manual quit; every launch after that is covered.
