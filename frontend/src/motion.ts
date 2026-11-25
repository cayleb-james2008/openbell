// Central Emil Kowalski Motion Contract v2.0 presets for OpenBell.
// Matches CSS tokens in tokens.css (--dur-press 150ms, --dur-reveal 180ms, --dur-swap 200ms, --dur-enter/sheet 220ms).

export const ease = [0.23, 1, 0.32, 1] as [number, number, number, number];

export const dur = {
  press: 0.15,
  reveal: 0.18,
  swap: 0.2,
  enter: 0.22,
  sheet: 0.22,
};

/** Button / control press tween. */
export const press = { duration: dur.press, ease };

/** Interruptible spring for active nav rails and segmented tabs. */
export const glide = { type: "spring" as const, stiffness: 450, damping: 35, mass: 0.7 };

/** Modal / dialog entrance spring (subtle 0.05 bounce, zero scale(0)). */
export const open = { type: "spring" as const, bounce: 0.05, duration: 0.3 };

/** Fast opacity/y reveal for panels, scorecard stages, and status rows. */
export const reveal = { duration: dur.reveal, ease };

/** Zero-duration preset for keyboard shortcuts and 100+/day interactions. */
export const instant = { duration: 0 };
