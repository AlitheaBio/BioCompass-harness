import type { IconProps } from './icons/props.ts'

/** Native viewBox of {@link FISH_LOGO_PATH} (width and height in user units). */
export const FISH_LOGO_VIEWBOX = { width: 100, height: 100 }

/** Outer boundary path data for consumers composing masks or overlays around the BioCompass orb geometry. */
export const FISH_LOGO_PATH = 'M50 8A42 42 0 1 1 50 92A42 42 0 1 1 50 8Z'

/**
 * Render the BioCompass Halo-Orb brand mark.
 * @param props.size - width in px (default 24; height keeps the 1:1 ratio).
 * @param props.className - extra class for layout placement.
 * @returns the brand mark svg (aria-hidden; pair with the wordmark or text label for accessibility).
 */
export function FishLogo({ size = 24, className }: IconProps) {
  return (
    <svg
      width={size}
      height={(size * FISH_LOGO_VIEWBOX.height) / FISH_LOGO_VIEWBOX.width}
      className={className}
      viewBox={`0 0 ${FISH_LOGO_VIEWBOX.width} ${FISH_LOGO_VIEWBOX.height}`}
      fill="none"
      aria-hidden="true"
    >
      <circle cx="50" cy="50" r="42" stroke="currentColor" strokeWidth="5" />
      <path d="M 8 50 A 42 27.72 0 0 1 92 50" stroke="currentColor" strokeWidth="3.6" strokeLinecap="round" strokeOpacity="0.88" />
      <path d="M 8 50 A 42 27.72 0 0 0 92 50" stroke="currentColor" strokeWidth="3.8" strokeLinecap="round" strokeOpacity="0.88" />
      <path d="M 8 50 A 42 3.86 0 0 0 92 50" stroke="#F56E3D" strokeWidth="5.6" strokeLinecap="round" />
      <g transform="translate(50 50) rotate(38)">
        <polygon
          points="0,-32 10.5,0 0,32 -10.5,0"
          fill="var(--dsw-alias-bg-base, #ffffff)"
          stroke="var(--dsw-alias-bg-base, #ffffff)"
          strokeWidth="1.2"
          strokeLinejoin="round"
        />
        <polygon points="0,-29.2 8,0 -8,0" fill="#F56E3D" />
        <polygon points="0,29.2 8,0 -8,0" fill="currentColor" />
        <circle cx="0" cy="0" r="2.6" fill="var(--dsw-alias-bg-base, #ffffff)" />
      </g>
    </svg>
  )
}
