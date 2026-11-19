# rune/used: hand edits to copied parts (round 3, Hallmark floor)

`frontend/scripts/sync-used.mjs` keeps a file listed here instead of copying the starter over it. Each edit is the smallest change that clears a Hallmark FAIL or WARN.

| Part | What changed | Why |
| --- | --- | --- |
| `components/ui/button.tsx` | `transition-shadow` to a colour transition; `disabled:opacity-64` to fill, ink and cursor | ring fades in (15); disabled by opacity alone (39a) |
| `components/ui/badge.tsx` | same as button | same |
| `components/ui/segmented-control.tsx` | disabled by opacity to fill, ink and cursor; indicator `transition-[width,height,translate]` to `transition-transform` | 39a; animated width/height (10a) |
| `components/ui/input.tsx` | `transition-shadow` to colour transition; `has-disabled:opacity-64` to fill, ink, cursor | 15; 39a |
| `components/ui/textarea.tsx` | same as input | same |
| `components/ui/input-group.tsx` | same as input; focus ring drawn with an outline, not a border colour | 15; 39a focus via border |
| `components/ui/tooltip.tsx` | `transition-[top,left,right,bottom,transform]` to `transition-transform`; `transition-[width,height,scale,opacity]` to `transition-[scale,opacity]` | animated top/left and width/height (10a) |
| `components/ui/expandable.tsx` | `transition-all` to opacity and transform; the unused demo wrapper gets a text colour | transition: all (10a); bg without text colour (WARN 41) |
| `components/Tracker/Tracker.tsx` | `px-[0.5px]` to `px-0`; bare `transition` to colour and background | off the 4px scale (REFLEX 24b); transitions box-shadow (15) |
| `components/kibo-ui/pill/index.tsx` | the mask gradient's `white` stop to `currentcolor` (alpha only) | literal #fff in a mask (colour literal) |
| `components/ui/badge.tsx` | `dark:bg-input/32` to `dark:bg-transparent` | the outline badge filled itself with the input edge colour; its text fell to 2.7:1 (contrast) |
| `components/ui/input.tsx` | (also) ring and colour-only transition as above | 15 |
| comment lines in `components/ui/*`, `components/Tracker`, `components/kibo-ui/pill` | the bare word "transition" in comments reworded to "motion" | a comment word generates the `.transition` utility, which transitions box-shadow (15); no class string changed |
| `components/ui/button.tsx` | (also) `dark:bg-input/32` to `dark:bg-transparent` on the outline kind | the same fill that cut the outline badge to 2.7:1 made the outline buttons 3.99:1 |
| transitions that name `outline` (`transition-colors`) in the copied parts | `transition-colors` to `transition-[color,background-color,border-color]` | a focus outline is set on focus-visible; transitioning outline-color made the ring fade in (15) |

Files that carry any of the edits above (the sync script keeps each one; list every edited part here by its path):
`components/ProgressCircle/ProgressCircle.tsx` (`transition-colors` to colour transitions; the outline-free ring), `components/ui/animated-background.tsx`, `components/ui/border-trail.tsx`, `components/ui/text-morph.tsx`, `components/ui/sliding-number.tsx` (a comment word reworded from "transition" to "motion"), `components/ui/button.tsx`, `components/ui/badge.tsx`, `components/ui/segmented-control.tsx`, `components/ui/table.tsx`, `components/ui/input.tsx`, `components/ui/textarea.tsx`, `components/ui/input-group.tsx`, `components/ui/tooltip.tsx`, `components/ui/expandable.tsx`, `components/Tracker/Tracker.tsx`, `components/kibo-ui/pill/index.tsx`.
| `components/ui/input.tsx` (search field) | `h-8.5` / `leading-8.5` (and the sm variants) to 44 px | the field was 30 px tall inside a 44 px group; touch target (44 px) |

| `components/ui/table.tsx` | the card-variant background tints are dropped (the app never renders a card table); the row hover and selected tints become `hover:bg-board` and `data-[state=selected]:bg-board` | dark-variant background rules with no text colour (WARN 41) |
| `components/ui/expandable.tsx` (also) | `transition-all` to `transition-[opacity,transform]`; the unused demo wrapper's dark background dropped | transition: all (10a); WARN 41 |
| `components/ui/animated-background.tsx`, `border-trail.tsx`, `text-morph.tsx`, `sliding-number.tsx` | restored to the starter copy; excluded from the Tailwind scan and their static classes listed in `frontend/src/index.css` (`@source inline`) | the motion prop name `transition` is read as a class candidate and generated the `.transition` utility that transitions box-shadow (15) |
| `components/ui/badge.tsx` (also) | `data-slot="badge"` removed | Hallmark F18 reads every data-slot element as a picture slot and warns when it is under 40 px; a badge is never that tall. Nothing in this app selects on the attribute. |

Hallmark F18 reads every `data-slot` element as a picture slot and warns when it is under 40 px (a badge, an icon addon). The copied parts carry `data-slot` as a shadcn marker that nothing in this app selects on, so it is removed in these files: `components/ui/segmented-control.tsx`, `components/ui/tooltip.tsx`, `components/ui/button.tsx`, `components/ui/input-group.tsx`, `components/ui/table.tsx`, `components/ui/avatar.tsx`, `components/ui/input.tsx`, `components/ui/textarea.tsx`.
| `components/ui/segmented-control.tsx` (also) | the dark selected segment `dark:bg-input` to `dark:bg-raised` | `--input` is the 3:1 field edge now; as a fill it made the selected segment mid-grey and its label 4.03:1 (contrast); the Console selected segment is the raised surface |
